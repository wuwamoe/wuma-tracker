use crate::native_collector::{NativeCollector, collection_loop};
use crate::offsets::TrackerConfig;
use crate::peer_manager::PeerManager;
use crate::room_code_generator::generate_room_code_base36;
use crate::signaling_handler::SignalingHandler;
use crate::types::{CollectorMessage, GlobalState, RtcSignal, SignalPacket, SupervisorCommand};
use crate::util;
use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

struct CollectorState {
    instance: Arc<Mutex<Option<NativeCollector>>>,
    cancel: Option<CancellationToken>,
}

pub struct RtcSupervisor {
    signaling_handler: SignalingHandler,
    peer_manager: PeerManager,
    collector_state: CollectorState,
    offsets: Arc<Mutex<Option<TrackerConfig>>>,
    sh_pm_rx: mpsc::Receiver<SignalPacket>,
    collector_rx: mpsc::Receiver<CollectorMessage>,
    // (url, attempt_count) — 외부 연결 자동 재연결 채널
    reconnect_tx: mpsc::Sender<(String, u32)>,
    reconnect_rx: mpsc::Receiver<(String, u32)>,
}

impl RtcSupervisor {
    pub fn new(offsets: Arc<Mutex<Option<TrackerConfig>>>) -> Self {
        let (sh_pm_tx, sh_pm_rx) = mpsc::channel(128);
        let (pm_sh_tx, pm_sh_rx) = mpsc::channel(128);
        let (_collector_tx, collector_rx) = mpsc::channel(128);
        let (reconnect_tx, reconnect_rx) = mpsc::channel(4);

        let signaling_handler = SignalingHandler::new(sh_pm_tx, pm_sh_rx);
        let peer_manager = PeerManager::new(pm_sh_tx);

        Self {
            signaling_handler,
            peer_manager,
            collector_state: CollectorState {
                instance: Arc::new(Mutex::new(None)),
                cancel: None,
            },
            offsets,
            sh_pm_rx,
            collector_rx,
            reconnect_tx,
            reconnect_rx,
        }
    }

    pub async fn run(
        &mut self,
        app_handle: AppHandle,
        ip: String,
        port: u16,
        mut command_rx: mpsc::Receiver<SupervisorCommand>,
        shutdown_token: CancellationToken,
    ) -> Result<(), String> {
        log::info!("Starting RtcSupervisor...");

        if let Err(e) = self
            .signaling_handler
            .restart_local_server(app_handle.clone(), ip, port)
            .await
        {
            log::error!("Failed to start SignalingHandler: {}", e);
            if let Err(emit_err) = app_handle.emit("report-error-toast", format!("서버 시작 실패 (포트 {}): {}", port, e)) {
                log::error!("Failed to emit error to frontend: {}", emit_err);
            }
        } else {
            log::info!("SignalingHandler started.");
        }

        log::info!("RtcSupervisor is now running. Waiting for events...");
        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    log::info!("Shutdown signal received. Shutting down RtcSupervisor.");
                    break;
                }

                Some(event) = self.sh_pm_rx.recv() => {
                    log::debug!("Supervisor received event: {:?}", event);
                    let client_id = event.from.clone();
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
                        _ => self.peer_manager.handle_signaling_message(event).await,
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
                            }
                        }
                        CollectorMessage::Terminated => {
                            log::error!("Process terminated. Detaching...");
                            self.detach_process().await;
                            util::mutate_global_state(&app_handle, |s| s.proc_state = 0);
                        }
                        CollectorMessage::TemporalError(e) => {
                            if let Err(e) = app_handle.emit("handle-tracker-error", e.clone()) {
                                log::error!("Error sending collector error to frontend: {}", e);
                            }
                        }
                        CollectorMessage::OffsetFound(name) => {
                            log::info!("Successfully found and locked onto offset: {}", name);
                            util::mutate_global_state(&app_handle, |s| s.active_offset_name = Some(name.clone()));
                        }
                    }
                }

                Some((url, attempt)) = self.reconnect_rx.recv() => {
                    log::info!("[External] 자동 재연결 시도 {}/{}", attempt, crate::signaling_handler::MAX_RECONNECT_ATTEMPTS);
                    let code = url.trim_end_matches("?role=server")
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .to_string();
                    match self.signaling_handler.connect_to_external_server(
                        app_handle.clone(),
                        url,
                        0, // 성공 시 attempt 리셋 → 다음 끊김 시 처음부터
                        self.reconnect_tx.clone(),
                    ).await {
                        Ok(_) => {
                            util::mutate_global_state(&app_handle, |s| s.external_connection_code = Some(code));
                            log::info!("[External] 자동 재연결 성공");
                        }
                        Err(e) => {
                            log::error!("[External] 자동 재연결 실패 ({}회 시도 후 포기): {}", attempt, e);
                        }
                    }
                }

                Some(command) = command_rx.recv() => {
                    match command {
                        SupervisorCommand::AttachProcess(proc_name, responder) => {
                            let result = self.attach_process(app_handle.clone(), &proc_name).await;
                            let _ = responder.send(result);
                        }
                        SupervisorCommand::LaunchAndAttach(path, responder) => {
                            let result = self.do_launch_and_attach(app_handle.clone(), &path).await;
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
                                if let Err(emit_err) = app_handle.emit("report-error-toast", format!("서버 시작 실패 (포트 {}): {}", port, e)) {
                                    log::error!("Failed to emit error to frontend: {}", emit_err);
                                }
                            }
                        }
                        SupervisorCommand::RestartExternalConnection(responder) => {
                            let code = generate_room_code_base36();
                            let url = format!("wss://concourse.wuwa.moe/{}?role=server", code);
                            match self.signaling_handler.connect_to_external_server(
                                app_handle.clone(),
                                url,
                                0,
                                self.reconnect_tx.clone(),
                            ).await {
                                Ok(_) => {
                                    let code_clone = code.clone();
                                    util::mutate_global_state(&app_handle, |s| s.external_connection_code = Some(code_clone));
                                    let _ = responder.send(Ok(code));
                                }
                                Err(e) => {
                                    log::error!("Restart external signaling server failed: {}", e);
                                    util::mutate_global_state(&app_handle, |s| s.external_connection_code = None);
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

    pub async fn do_launch_and_attach(&mut self, app_handle: AppHandle, path: &str) -> Result<(), String> {
        #[cfg(windows)]
        {
            if self.collector_state.instance.lock().await.is_some() {
                self.detach_process().await;
            }

            let path_str = path.to_string();
            let cache_dir = app_handle
                .path()
                .app_config_dir()
                .map_err(|e| e.to_string())?;

            let scan_config = self.offsets.lock().await
                .as_ref()
                .map(|c| c.gworld_scan.clone());

            let win_proc = tokio::task::spawn_blocking(move || {
                crate::game_launcher::launch_and_create_proc(&path_str, cache_dir, scan_config)
            })
            .await
            .map_err(|e| format!("태스크 실패: {}", e))?
            .map_err(|e| e.to_string())?;

            let collector = NativeCollector::from_win_proc(win_proc);
            *self.collector_state.instance.lock().await = Some(collector);
            log::info!("Game launched and attached via process handle.");
            self.try_start_collector().await;
            util::mutate_global_state(&app_handle, |s| s.proc_state = 1);
            Ok(())
        }

        #[cfg(target_os = "macos")]
        {
            crate::game_launcher::launch_game(path).map_err(|e| e.to_string())
        }
    }

    pub async fn attach_process(&mut self, app_handle: AppHandle, proc_name: &str) -> Result<(), String> {
        if self.collector_state.instance.lock().await.is_some() {
            log::info!("Process is already attached.");
            return Ok(());
        }

        let cache_dir = app_handle
            .path()
            .app_config_dir()
            .map_err(|e| e.to_string())?;

        let scan_config = self.offsets.lock().await
            .as_ref()
            .map(|c| c.gworld_scan.clone());

        match NativeCollector::new(proc_name, cache_dir, scan_config).await {
            Ok(collector) => {
                *self.collector_state.instance.lock().await = Some(collector);
                log::info!("Process attached successfully.");
                self.try_start_collector().await;
                util::mutate_global_state(&app_handle, |s| s.proc_state = 1);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn detach_process(&mut self) {
        log::info!("Detaching from process by external signal.");
        self.try_stop_collector().await;
        *self.collector_state.instance.lock().await = None;
    }

    async fn try_start_collector(&mut self) {
        if self.collector_state.instance.lock().await.is_some()
            && self.collector_state.cancel.is_none()
        {
            log::info!("Conditions met. Starting collection loop.");
            let cancel = CancellationToken::new();
            self.collector_state.cancel = Some(cancel.clone());

            let (pm_tx, pm_rx) = mpsc::channel(100);
            self.collector_rx = pm_rx;

            tokio::spawn(collection_loop(
                self.collector_state.instance.clone(),
                pm_tx,
                cancel,
                self.offsets.clone(),
            ));
        }
    }

    async fn try_stop_collector(&mut self) {
        if let Some(cancel) = self.collector_state.cancel.take() {
            log::info!("Stopping collection loop.");
            cancel.cancel();
        }
    }
}
