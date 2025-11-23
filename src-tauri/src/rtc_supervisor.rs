use crate::native_collector::{NativeCollector, collection_loop};
use crate::peer_manager::PeerManager; // 이전 코드에서 정의
use crate::room_code_generator::generate_room_code_base36;
use crate::signaling_handler::SignalingHandler;
use crate::types::{CollectorMessage, GlobalState, RtcSignal, SignalPacket, SupervisorCommand};
use crate::util;
use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, mpsc, oneshot};

struct CollectorState {
    instance: Arc<Mutex<Option<NativeCollector>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

/// 모든 RTC 관련 컴포넌트를 총괄하고 오케스트레이션하는 최상위 구조체
pub struct RtcSupervisor {
    signaling_handler: SignalingHandler,
    peer_manager: PeerManager,
    // SignalingHandler -> PeerManager 이벤트 수신부
    collector_state: CollectorState,

    sh_pm_rx: mpsc::Receiver<SignalPacket>,
    collector_rx: mpsc::Receiver<CollectorMessage>,
}

impl RtcSupervisor {
    /// RtcSupervisor를 생성하고 모든 하위 컴포넌트를 초기화 및 연결합니다.
    pub fn new() -> Self {
        // 1. SignalingHandler와 PeerManager 간의 통신 채널을 생성합니다.
        // sh -> pm: IncomingSignal (새 클라이언트, 메시지, 연결 종료 등)
        // pm -> sh: OutgoingSignal (특정 클라이언트에게 메시지 전송 명령 등)
        let (sh_pm_tx, sh_pm_rx) = mpsc::channel(128);
        let (pm_sh_tx, pm_sh_rx) = mpsc::channel(128);

        // tokio::select의 정상 작동을 위해서 더미 채널 생성
        let (_collector_tx, collector_rx) = mpsc::channel(128);

        // 2. 각 컴포넌트를 생성합니다.
        let signaling_handler = SignalingHandler::new(
            sh_pm_tx, // 이벤트 송신부
            pm_sh_rx, // 명령 수신부
        );

        let peer_manager = PeerManager::new(
            pm_sh_tx, // 명령 송신부
        );

        Self {
            signaling_handler,
            peer_manager,
            collector_state: CollectorState {
                instance: Arc::new(Mutex::new(None)),
                shutdown_tx: None,
            },
            sh_pm_rx,
            collector_rx,
        }
    }

    /// 시스템의 메인 이벤트 루프를 시작하고 전체 시스템을 구동합니다.
    /// 이 함수는 프로그램이 종료될 때까지 실행됩니다.
    pub async fn run(
        &mut self,
        app_handle: AppHandle,
        ip: String,
        port: u16,
        mut command_rx: mpsc::Receiver<SupervisorCommand>,
        mut shutdown_signal: oneshot::Receiver<()>,
    ) -> Result<(), String> {
        log::info!("Starting RtcSupervisor...");

        if let Err(e) = self.signaling_handler
            .restart_local_server(app_handle.clone(), ip, port)
            .await 
        {
            log::error!("Failed to start SignalingHandler (Port might be in use): {}", e);
        
            let error_message = format!("서버 시작 실패 (포트 {}): {}", port, e);
            
            if let Err(emit_err) = app_handle.emit("report-error-toast", error_message) {
                log::error!("Failed to emit error to frontend: {}", emit_err);
            }
        } else {
            log::info!("SignalingHandler started.");
        }

        // 3. RtcSupervisor의 메인 이벤트 루프
        log::info!("RtcSupervisor is now running. Waiting for events...");
        loop {
            tokio::select! {
                // 외부로부터의 종료 신호 감지
                _ = &mut shutdown_signal => {
                    log::info!("Shutdown signal received. Shutting down RtcSupervisor.");
                    break;
                }

                // SignalingHandler로부터 오는 이벤트를 수신
                Some(event) = self.sh_pm_rx.recv() => {
                    log::debug!("Supervisor received event: {:?}", event);
                    let client_id = event.from.clone();
                    // 수신한 이벤트를 PeerManager의 해당 핸들러에 전달
                    let result = match event.msg {
                        RtcSignal::NewPeer => {
                            let result = self.peer_manager.handle_new_external_client(client_id).await;
                            self.try_start_collector().await;
                            result
                        }
                        RtcSignal::PeerLeft => {
                            self.peer_manager.handle_client_disconnect(client_id).await
                        }
                        RtcSignal::NewLocalPeer => {
                            let result = self.peer_manager.handle_new_local_client(client_id).await;
                            self.try_start_collector().await;
                            result
                        }
                        _ => {
                            self.peer_manager.handle_signaling_message(event).await
                        }
                    };

                    if let Err(e) = result {
                        log::error!("Error handling event: {}", e);
                    }
                }

                Some(msg) = self.collector_rx.recv() => {
                    match msg {
                        CollectorMessage::Data(player_info) => {
                            if let Err(e) = app_handle.emit("handle-location-change", player_info.clone()) {
                                log::error!("Error sending location to frontend: {}", e);
                            }
                            if let Err(e) = self.peer_manager.broadcast_data(&player_info).await {
                                log::error!("Error broadcasting data: {}", e);
                            };
                        }
                        CollectorMessage::Terminated => {
                            log::error!("Process terminated. Detaching...");
                            self.detach_process().await;
                            let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                                proc_state: 0,
                                ..old
                            }).await;
                        }
                        CollectorMessage::TemporalError(e) => {
                            if let Err(e) = app_handle.emit("handle-tracker-error", e.clone()) {
                                log::error!("Error sending collector error to frontend: {}", e);
                            };
                        }
                        CollectorMessage::OffsetFound(name) => {
                            log::info!("Successfully found and locked onto offset: {}", name);
                            let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                                active_offset_name: Some(name.clone()),
                                ..old
                            }).await;
                        }
                    }
                }

                Some(command) = command_rx.recv() => {
                    // log::info!("Supervisor received command: {:?}", command);
                    match command {
                        SupervisorCommand::AttachProcess(proc_name, responder) => {
                            let result = self.attach_process(app_handle.clone(), &proc_name).await;
                            let _ = responder.send(result);
                        }
                        SupervisorCommand::RestartSignalingServer => {
                            let config = util::get_config(app_handle.clone()).await.unwrap_or_default();
                            if let Err(e) = self.signaling_handler.restart_local_server(
                                app_handle.clone(),
                                config.ip.unwrap_or(String::from("127.0.0.1")),
                                config.port.unwrap_or(46821),
                            ).await {
                                log::error!("Restart local signaling server failed: {}", e);

                                let error_message = format!("서버 시작 실패 (포트 {}): {}", port, e);
            
                                if let Err(emit_err) = app_handle.emit("report-error-toast", error_message) {
                                    log::error!("Failed to emit error to frontend: {}", emit_err);
                                }
                            };
                        }
                        SupervisorCommand::RestartExternalConnection(responder) => {
                            let code = generate_room_code_base36();
                            match self.signaling_handler.connect_to_external_server(app_handle.clone(), format!("wss://concourse.wuwa.moe/{}?role=server", code.clone())).await {
                                Ok(_) => {
                                    let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                                        external_connection_code: Some(code.clone()),
                                        ..old
                                    }).await;
                                    let _ = responder.send(Ok(code));
                                }
                                Err(e) => {
                                    log::error!("Restart external signaling server failed: {}", e);
                                    let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                                        external_connection_code: None,
                                        ..old
                                    }).await;
                                    let _ = responder.send(Err(e));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
    // --- Collector 생명주기 메소드 ---

    /// 외부에서 프로세스 attach를 명령하기 위한 API
    pub async fn attach_process(
        &mut self,
        app_handle: AppHandle,
        proc_name: &str,
    ) -> Result<(), String> {
        // 이미 attach 되어 있다면 아무것도 하지 않음
        if self.collector_state.instance.lock().await.is_some() {
            log::info!("Process is already attached.");
            return Ok(());
        }

        // log::info!("Attempting to attach to process: {}", proc_name);
        match NativeCollector::new(proc_name).await {
            Ok(collector) => {
                *self.collector_state.instance.lock().await = Some(collector);
                log::info!("Process attached successfully.");
                // attach 성공 후, collection_loop 시작 조건을 즉시 확인
                self.try_start_collector().await;
                let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                    proc_state: 1,
                    ..old
                })
                .await;
                Ok(())
            }
            Err(e) => {
                // log::error!("Failed to attach to process: {}", e);
                Err(e.to_string())
            }
        }
    }
    pub async fn detach_process(&mut self) {
        log::info!("Detaching from process by external signal.");
        // 먼저 실행 중인 루프를 중지
        self.try_stop_collector().await;
        // 그 다음 인스턴스를 제거
        *self.collector_state.instance.lock().await = None;
    }

    async fn try_start_collector(&mut self) {
        // 조건: 프로세스 attach 상태 AND 루프가 현재 미실행
        if self.collector_state.instance.lock().await.is_some()
            && self.collector_state.shutdown_tx.is_none()
        {
            log::info!("Conditions met. Starting collection loop.");
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            self.collector_state.shutdown_tx = Some(shutdown_tx);

            let (pm_tx, pm_rx) = mpsc::channel(100);
            self.collector_rx = pm_rx;

            tokio::spawn(collection_loop(
                self.collector_state.instance.clone(),
                pm_tx,
                shutdown_rx,
            ));
        }
    }

    /// collection_loop가 실행 중일 경우, 종료 신호를 보내는 함수
    async fn try_stop_collector(&mut self) {
        if let Some(shutdown_tx) = self.collector_state.shutdown_tx.take() {
            log::info!("Stopping collection loop.");
            let _ = shutdown_tx.send(());
        }
    }
}
