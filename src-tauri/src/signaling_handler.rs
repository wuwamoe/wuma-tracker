use crate::types::{ExternalSession, RtcSignal, SERVER_ID, SignalPacket, WsRouteInfo};
use crate::util;
use anyhow::Result;
use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::channel::mpsc as futures_mpsc;
use futures::{SinkExt, StreamExt};
use tokio::time::timeout;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

const PING_INTERVAL_SECS: u64 = 15;
pub(crate) const MAX_RECONNECT_ATTEMPTS: u32 = 3;

pub(crate) struct SignalingHandler {
    server_cancel: Option<CancellationToken>,
    sh_pm_tx: Arc<mpsc::Sender<SignalPacket>>,
    pm_sh_rx: Option<mpsc::Receiver<SignalPacket>>,
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
            server_cancel: None,
            sh_pm_tx: Arc::new(sh_pm_tx),
            pm_sh_rx: Some(pm_sh_rx),
            external_session: Arc::new(Mutex::new(None)),
            switching_table: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn restart_local_server(
        &mut self,
        app_handle: AppHandle,
        ip: String,
        port: u16,
    ) -> Result<(), String> {
        if let Some(cancel) = self.server_cancel.take() {
            log::info!("Restarting signaling server. Sending shutdown signal to the old instance...");
            cancel.cancel();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        match self.start_local_server_impl(ip, port).await {
            Ok(addr) => {
                util::mutate_global_state(&app_handle, |s| {
                    s.server_state = 1;
                    s.connection_url = Some(addr.clone());
                });
            }
            Err(err) => {
                util::mutate_global_state(&app_handle, |s| {
                    s.server_state = 0;
                    s.connection_url = None;
                });
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
            .map_err(|e| format!("통신 서버 시작 실패: {}", e))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("로컬 주소를 확인하는데 실패했습니다: {}", e))?
            .to_string();
        log::info!("listening on {}", addr);

        let app = Router::new()
            .route("/", get(Self::websocket_handler))
            .with_state(Arc::new(LocalAxumState {
                sh_pm_tx: self.sh_pm_tx.clone(),
                switching_table: self.switching_table.clone(),
            }))
            .layer(cors);

        let cancel = CancellationToken::new();
        self.server_cancel = Some(cancel.clone());

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { cancel.cancelled().await })
                .await
                .unwrap();
        });
        Ok(addr)
    }

    pub async fn connect_to_external_server(
        &self,
        app_handle: AppHandle,
        url: String,
        attempt: u32,
        reconnect_tx: mpsc::Sender<(String, u32)>,
    ) -> Result<(), String> {
        let mut external_session_guard = self.external_session.lock().await;
        if let Some(old_session) = external_session_guard.take() {
            log::info!("Shutting down previous external connection session...");
            old_session.cancel.cancel();
            old_session.shutdown_handle.abort();
        }

        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| format!("외부 시그널링 서버 연결 실패: {}", e))?;

        log::info!("Successfully connected to external signaling server: {}", url);

        let (ws_sender, mut ws_receiver) = ws_stream.split();
        let (unbounded_tx, unbounded_rx) = futures_mpsc::unbounded();
        let write_task = tokio::spawn(unbounded_rx.map(Ok).forward(ws_sender));

        let sh_pm_tx = self.sh_pm_tx.clone();
        let switching_table = self.switching_table.clone();
        let ping_tx = unbounded_tx.clone();

        let read_task = tokio::spawn(async move {
            let mut pong_pending = false;
            loop {
                match timeout(Duration::from_secs(PING_INTERVAL_SECS), ws_receiver.next()).await {
                    Err(_) => {
                        if pong_pending {
                            log::warn!("[External] Pong 미수신 → 연결 끊김 감지");
                            break;
                        }
                        log::debug!("[External] {}초 비활성 → Ping 전송", PING_INTERVAL_SECS);
                        if ping_tx.unbounded_send(TungsteniteMessage::Ping(vec![].into())).is_err() {
                            log::error!("[External] Ping 전송 실패: 연결 종료");
                            break;
                        }
                        pong_pending = true;
                    }
                    Ok(Some(Ok(msg))) => {
                        pong_pending = false;
                        match msg {
                            TungsteniteMessage::Text(text) => {
                                if let Ok(packet) = serde_json::from_str::<SignalPacket>(&text) {
                                    let client_id = packet.from.clone();
                                    match packet.msg {
                                        RtcSignal::NewPeer => {
                                            log::info!("[External] New peer '{}' registered.", client_id);
                                            switching_table.lock().await.insert(client_id.clone(), WsRouteInfo::External);
                                        }
                                        RtcSignal::PeerLeft => {
                                            log::info!("[External] Peer '{}' left.", client_id);
                                            switching_table.lock().await.remove(&client_id);
                                        }
                                        _ => {}
                                    }
                                    if sh_pm_tx.send(packet).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            TungsteniteMessage::Pong(_) => {
                                log::debug!("[External] Pong 수신");
                            }
                            TungsteniteMessage::Close(_) => {
                                log::info!("[External] 서버로부터 Close 수신");
                                break;
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => {
                        log::info!("[External] 스트림 종료");
                        break;
                    }
                    Ok(Some(Err(e))) => {
                        log::error!("[External] 수신 오류: {}", e);
                        break;
                    }
                }
            }
        });

        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        let session_task_handle = tokio::spawn(async move {
            tokio::select! {
                _ = read_task => {}
                _ = write_task => {}
            }
            log::info!("[External] 세션 종료");
            util::mutate_global_state(&app_handle, |s| s.external_connection_code = None);

            // 의도적 종료(cancel)가 아닌 경우에만 자동 재연결 시도
            if !cancel_for_task.is_cancelled() && attempt < MAX_RECONNECT_ATTEMPTS {
                let delay = std::cmp::min(5 * 2u64.pow(attempt), 60);
                log::info!("[External] {}초 후 재연결 시도 ({}/{})", delay, attempt + 1, MAX_RECONNECT_ATTEMPTS);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(delay)) => {
                        let _ = reconnect_tx.send((url, attempt + 1)).await;
                    }
                    _ = cancel_for_task.cancelled() => {
                        log::info!("[External] 재연결 대기 중 취소됨");
                    }
                }
            }
        });

        *external_session_guard = Some(ExternalSession {
            ws_sender: unbounded_tx,
            shutdown_handle: session_task_handle,
            cancel,
        });

        Ok(())
    }

    async fn start_command_processor(&mut self) {
        if let Some(mut rx) = self.pm_sh_rx.take() {
            let switching_table = self.switching_table.clone();
            let external_session = self.external_session.clone();
            tokio::spawn(async move {
                while let Some(SignalPacket { from, to, msg }) = rx.recv().await {
                    let route_info_map = switching_table.lock().await;
                    if let Some(route_info) = route_info_map.get(&to) {
                        match route_info {
                            WsRouteInfo::Local(local_sender) => {
                                let sender = local_sender.clone();
                                drop(route_info_map);
                                let _ = sender.send(msg).await;
                            }
                            WsRouteInfo::External => {
                                let external_session_locked = external_session.lock().await;
                                drop(route_info_map);
                                if let Some(session) = &*external_session_locked {
                                    let packet = SignalPacket { from, to, msg };
                                    if let Ok(packet_str) = serde_json::to_string(&packet) {
                                        let _ = session.ws_sender.unbounded_send(
                                            TungsteniteMessage::Text(packet_str.into()),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        log::warn!("No route found for client ID: {}", to);
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

        let (tx, mut rx) = mpsc::channel::<RtcSignal>(100);

        state
            .switching_table
            .lock()
            .await
            .insert(client_id.clone(), WsRouteInfo::Local(tx));

        if let Err(e) = state
            .sh_pm_tx
            .send(SignalPacket {
                from: client_id.clone(),
                to: SERVER_ID.to_string(),
                msg: RtcSignal::NewLocalPeer,
            })
            .await
        {
            log::error!("[{}] Failed to send NewClient signal: {}", client_id, e);
            state.switching_table.lock().await.remove(&client_id);
            return;
        }
        log::info!("[{}] New client registered.", client_id);

        loop {
            tokio::select! {
                Some(signal) = rx.recv() => {
                    if let Ok(json) = serde_json::to_string(&signal) {
                        if ws_sender.send(Message::Text(json.into())).await.is_err() {
                            log::warn!("[{}] Failed to send message to websocket, client likely disconnected.", client_id);
                            break;
                        }
                    }
                }
                Some(Ok(message)) = ws_receiver.next() => {
                    match message {
                        Message::Text(text) => {
                            if let Ok(msg) = serde_json::from_slice::<RtcSignal>(text.as_bytes()) {
                                if state.sh_pm_tx.send(SignalPacket { from: client_id.clone(), to: SERVER_ID.to_string(), msg }).await.is_err() {
                                    log::error!("[{}] Failed to send Message signal.", client_id);
                                }
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                else => break,
            }
        }

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
