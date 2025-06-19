use crate::types::{ExternalSession, GlobalState, RtcSignal, SignalPacket, WsRouteInfo, SERVER_ID};
use crate::util;
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use std::collections::HashMap;
use std::string::ToString;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::{mpsc, oneshot, Mutex};
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;
use futures::channel::mpsc as futures_mpsc;
use crate::room_code_generator::generate_room_code_base36;

pub(crate) struct SignalingHandler {
    // Axum 서버 종료를 위한 Sender
    localhost_server_shutdown_tx: Option<oneshot::Sender<()>>,
    // PeerManager로 이벤트를 보내는 채널
    sh_pm_tx: Arc<mpsc::Sender<SignalPacket>>,
    // PeerManager로부터 명령을 받는 채널
    pm_sh_rx: Option<mpsc::Receiver<SignalPacket>>,
    // "스위칭 테이블": ClientId와 해당 클라이언트로 메시지를 보낼 Sender를 매핑
    switching_table: Arc<Mutex<HashMap<String, WsRouteInfo>>>,
    external_session: Arc<Mutex<Option<ExternalSession>>>,
}

struct LocalAxumState {
    sh_pm_tx: Arc<mpsc::Sender<SignalPacket>>,
    switching_table: Arc<Mutex<HashMap<String, WsRouteInfo>>>,
}

impl SignalingHandler {
    pub fn new(
        sh_pm_tx: mpsc::Sender<SignalPacket>,
        pm_sh_rx: mpsc::Receiver<SignalPacket>,
    ) -> Self {
        Self {
            // Axum 서버는 아직 실행 전이므로 shutdown_tx는 None으로 초기화
            localhost_server_shutdown_tx: None,
            // 받은 Sender는 여러 웹소켓 태스크에서 공유해야 하므로 Arc로 감쌉니다.
            sh_pm_tx: Arc::new(sh_pm_tx),
            // 받은 Receiver는 start()에서 take()로 꺼내 써야 하므로 Option으로 감쌉니다.
            pm_sh_rx: Some(pm_sh_rx),
            external_session: Arc::new(Mutex::new(None)),
            // 연결 테이블은 비어있는 상태로 초기화합니다.
            switching_table: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn restart_local_server(
        &mut self,
        app_handle: AppHandle,
        ip: String,
        port: u16,
    ) -> Result<(), String> {
        if let Some(old_shutdown_tx) = self.localhost_server_shutdown_tx.take() {
            log::info!(
                "Restarting signaling server. Sending shutdown signal to the old instance..."
            );
            let _ = old_shutdown_tx.send(());

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        match self.start_local_server_impl(ip, port).await {
            Ok(addr) => {
                let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                    server_state: 1,
                    connection_url: Some(addr.clone()),
                    ..old
                })
                .await;
            }
            Err(err) => {
                let _ = util::mutate_global_state(app_handle.clone(), |old| GlobalState {
                    server_state: 1,
                    connection_url: None,
                    ..old
                })
                .await;
                return Err(err);
            }
        };
        self.start_command_processor().await;
        Ok(())
    }

    async fn start_local_server_impl(&mut self, ip: String, port: u16) -> Result<String, String> {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
        let listener = tokio::net::TcpListener::bind(&format!("{}:{}", ip, port))
            .await
            .map_err(|e| format!("통신 서버 시작 실패: {}", e).to_string())?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("로컬 주소를 확인하는데 실패했습니다: {}", e))?
            .to_string();
        log::info!("listening on {}", addr.clone());

        let app = Router::new()
            .route("/", get(Self::websocket_handler))
            .with_state(Arc::new(LocalAxumState {
                sh_pm_tx: self.sh_pm_tx.clone(),
                switching_table: self.switching_table.clone(),
            })).layer(cors);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.localhost_server_shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    shutdown_rx.await.ok();
                })
                .await
                .unwrap();
        });
        Ok(addr)
    }

    pub async fn connect_to_external_server(&self, url: String) -> Result<(), String> {
        // 1. 기존에 실행 중인 외부 연결 세션이 있다면 종료시킵니다.
        let mut external_session_guard = self.external_session.lock().await;
        if let Some(old_session) = external_session_guard.take() {
            log::info!("Shutting down previous external connection session...");
            // 이전 세션의 태스크를 중단시킵니다.
            old_session.shutdown_handle.abort();
        }

        // 2. 새로운 WebSocket 연결을 수립합니다.
        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| format!("외부 시그널링 서버 연결 실패: {}", e))?;

        log::info!("Successfully connected to external signaling server: {}", url);

        let (ws_sender, mut ws_receiver) = ws_stream.split();

        // 3. command_processor가 사용할 송신 채널(unbounded)을 설정합니다.
        //    이렇게 중간 채널을 두면 ws_sender(Sink)에 대한 Mutex 래핑 없이 사용할 수 있습니다.
        let (unbounded_tx, unbounded_rx) = futures_mpsc::unbounded();
        // 채널의 Receiver가 받은 메시지를 실제 WebSocket Sender로 전달하는 태스크
        let write_task = tokio::spawn(unbounded_rx.map(Ok).forward(ws_sender));

        // 4. 메시지 수신 및 라우팅 테이블 관리를 위한 태스크를 생성합니다.
        let sh_pm_tx = self.sh_pm_tx.clone();
        let switching_table = self.switching_table.clone();
        let read_task = tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_receiver.next().await {
                if let TungsteniteMessage::Text(text) = msg {
                    if let Ok(packet) = serde_json::from_str::<SignalPacket>(&text) {
                        let client_id = packet.from.clone();

                        // 워커가 보낸 메시지 종류에 따라 라우팅 테이블을 업데이트합니다.
                        match packet.msg {
                            RtcSignal::NewPeer => {
                                log::info!("[External] New peer '{}' registered in routing table.", client_id);
                                switching_table.lock().await.insert(client_id.clone(), WsRouteInfo::External);
                            }
                            RtcSignal::PeerLeft => {
                                log::info!("[External] Peer '{}' removed from routing table.", client_id);
                                switching_table.lock().await.remove(&client_id);
                            }
                            _ => {}
                        }

                        // 모든 패킷은 Supervisor에게 전달하여 PeerManager가 처리하도록 합니다.
                        if sh_pm_tx.send(packet).await.is_err() {
                            break;
                        }
                    }
                }
            }
            log::info!("External connection read task finished.");
        });

        // 5. 두 태스크를 함께 관리하고 종료할 수 있는 핸들을 만듭니다.
        let session_task_handle = tokio::spawn(async move {
            tokio::select! {
                _ = read_task => {},
                _ = write_task => {},
            }
            log::info!("External session tasks are closing.");
        });

        // 6. 새로운 외부 세션의 상태를 저장합니다.
        *external_session_guard = Some(ExternalSession {
            ws_sender: unbounded_tx,
            shutdown_handle: session_task_handle,
        });

        Ok(())
    }

    async fn start_command_processor(&mut self) {
        if let Some(mut rx) = self.pm_sh_rx.take() {
            let switching_table = self.switching_table.clone();
            let external_session = self.external_session.clone();
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    let route_info_map = switching_table.lock().await;

                    if let Some(route_info) = route_info_map.get(&command.to) {
                        match route_info {
                            WsRouteInfo::Local(local_sender) => {
                                if let Ok(msg_str) = serde_json::to_string(&command.msg) {
                                    // [수정] 라우팅 테이블에서 직접 Sender를 사용합니다.
                                    let _ = local_sender.send(msg_str).await;
                                }
                            }
                            WsRouteInfo::External => {
                                let external_session_locked = external_session.lock().await;
                                if let Some(session) = &*external_session_locked {
                                    if let Ok(packet_str) = serde_json::to_string(&command) {
                                        let _ = session.ws_sender.unbounded_send(TungsteniteMessage::Text(packet_str.into()));
                                    }
                                }
                            }
                        }
                    } else {
                        log::warn!("No route found for client ID: {}", command.to);
                    }
                }
            });
        } else {
            log::warn!("Command processor already started.");
        }
    }

    async fn websocket_handler(
        ws: WebSocketUpgrade,
        State(state): State<Arc<LocalAxumState>>,
    ) -> impl IntoResponse {
        log::info!("client connected");
        ws.on_upgrade(|socket| Self::websocket(socket, state))
    }

    async fn websocket(stream: WebSocket, state: Arc<LocalAxumState>) {
        let client_id = Uuid::new_v4().to_string();
        let (mut ws_sender, mut ws_receiver) = stream.split();

        let (tx, mut rx) = mpsc::channel::<String>(100);

        // 자신의 Sender를 "스위칭 테이블"에 등록
        state.switching_table.lock().await.insert(client_id.clone(), WsRouteInfo::Local(tx));

        // PeerManager에게 새 클라이언트 접속을 'ID만' 알림
        if let Err(e) = state
            .sh_pm_tx
            .send(SignalPacket {
                from: client_id.clone(),
                to: SERVER_ID.to_string(),
                msg: RtcSignal::NewPeer,
            })
            .await
        {
            log::error!("[{}] Failed to send NewClient signal: {}", client_id, e);
            state.switching_table.lock().await.remove(&client_id); // 등록 실패 시 즉시 제거
            return;
        }
        log::info!("[{}] New client registered.", client_id.clone());

        loop {
            tokio::select! {
                // SignalingHandler의 명령 처리기로부터 메시지를 받아 클라이언트에게 전송
                Some(msg_to_send) = rx.recv() => {
                    if ws_sender.send(Message::Text(msg_to_send.into())).await.is_err() {
                        log::warn!("[{}] Failed to send message to websocket, client likely disconnected.", client_id);
                        break;
                    }
                }
                // 클라이언트로부터 메시지를 받아 PeerManager에게 전송
                Some(Ok(message)) = ws_receiver.next() => {
                    match message {
                        Message::Text(text) => {
                            if let Ok(msg) = serde_json::from_slice::<RtcSignal>(text.as_bytes()) {
                                                      if state.sh_pm_tx.send(SignalPacket{from: client_id.clone(), to: SERVER_ID.to_string(), msg }).await.is_err() {
                                log::error!("[{}] Failed to send Message signal.", client_id);
                                // break;
                            }
                            }
                        }
                        Message::Close(_) => {
                            break;
                        }
                        _ => {}
                    }
                }
                else => { break; }
            }
        }

        // 루프 종료 후, "스위칭 테이블"에서 자신을 제거하고 PeerManager에게 접속 종료를 알림
        state.switching_table.lock().await.remove(&client_id);
        if state
            .sh_pm_tx
            .send(SignalPacket {
                from: client_id.clone(),
                to: SERVER_ID.to_string(),
                msg: RtcSignal::PeerLeft,
            })
            .await
            .is_err()
        {
            log::error!("[{}] Failed to send Disconnected signal.", client_id);
        }
        log::info!("[{}] Client disconnected and cleaned up.", client_id);
    }
}
