use crate::types::{GlobalState, RtcSignal, SignalPacket, SERVER_ID};
use crate::util;
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::string::ToString;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::{mpsc, oneshot, Mutex};
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

pub(crate) struct SignalingHandler {
    // Axum 서버 종료를 위한 Sender
    localhost_server_shutdown_tx: Option<oneshot::Sender<()>>,
    // PeerManager로 이벤트를 보내는 채널
    sh_pm_tx: Arc<mpsc::Sender<SignalPacket>>,
    // PeerManager로부터 명령을 받는 채널
    pm_sh_rx: Option<mpsc::Receiver<SignalPacket>>,
    // "스위칭 테이블": ClientId와 해당 클라이언트로 메시지를 보낼 Sender를 매핑
    connections: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
}

struct LocalAxumState {
    sh_pm_tx: Arc<mpsc::Sender<SignalPacket>>,
    connections: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
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
            // 연결 테이블은 비어있는 상태로 초기화합니다.
            connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start(
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

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        match self.start_localhost_server(ip, port).await {
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

    async fn start_localhost_server(&mut self, ip: String, port: u16) -> Result<String, String> {
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
                connections: self.connections.clone(),
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

    async fn start_command_processor(&mut self) {
        let connections = self.connections.clone();

        if let Some(mut rx) = self.pm_sh_rx.take() {
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    let id = command.to;
                    match serde_json::to_string(&command.msg) {
                        Ok(msg_str) => {
                            let connections_locked = connections.lock().await;
                            if let Some(sender) = connections_locked.get(&id) {
                                if let Err(e) = sender.send(msg_str).await {
                                    log::error!("[{}] Failed to forward message: {}", id, e);
                                }
                            } else {
                                log::warn!(
                                    "[{}] Tried to send message to non-existent client.",
                                    id
                                );
                            }
                        }
                        Err(e) => {
                            log::error!("[{}] Failed to serialize message: {}", id, e);
                        }
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
        state.connections.lock().await.insert(client_id.clone(), tx);

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
            state.connections.lock().await.remove(&client_id); // 등록 실패 시 즉시 제거
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
        state.connections.lock().await.remove(&client_id);
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
