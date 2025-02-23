use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinHandle,
};

use crate::{types::PlayerInfo, AppState};

pub struct ServerManager {
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl ServerManager {
    pub fn default() -> ServerManager {
        ServerManager {
            shutdown_tx: Option::None,
            handle: Option::None,
        }
    }

    pub async fn start(&mut self, app_handle: AppHandle, ip: String, port: u16) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        self.handle = Some(tokio::spawn(async move {
            // Set up application state for use with with_state().
            let (tx, _rx) = broadcast::channel(100);
            let client_count = Mutex::new(0);
            let ticker_handle: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

            let app = app_handle.clone();
            let app_state = Arc::new(AxumState {
                client_count,
                app_handle: app,
                tx,
                ticker_handle,
            });

            let app = Router::new()
                .route("/", get(Self::websocket_handler))
                .with_state(app_state);

            match tokio::net::TcpListener::bind(&format!("{}:{}", ip, port)).await {
                Ok(listener) => {
                    let addr = listener.local_addr().unwrap();
                    log::info!("listening on {}", addr);
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            shutdown_rx.await.ok();
                        })
                        .await
                        .unwrap();
                    log::info!("gracefully shutting down: {}", addr);
                }
                Err(_) => {
                    let app = app_handle.clone();
                    let _ = app
                        .dialog()
                        .message(
                            "통신 서버 시작 실패. IP 주소를 잘못 설정하였거나, 포트가 이미 사용 중인지 확인해주세요.",
                        )
                        .kind(MessageDialogKind::Error)
                        .title("오류")
                        .blocking_show();
                }
            }
        }));
    }

    pub async fn restart(&mut self, app_handle: AppHandle, ip: String, port: u16) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            self.handle.take().unwrap().await.ok();
        }
        self.start(app_handle, ip, port).await;
    }

    async fn websocket_handler(
        ws: WebSocketUpgrade,
        State(state): State<Arc<AxumState>>,
    ) -> impl IntoResponse {
        log::info!("client connected");
        ws.on_upgrade(|socket| Self::websocket(socket, state))
    }

    // This function deals with a single websocket connection, i.e., a single
    // connected client / user, for which we will spawn two independent tasks (for
    // receiving / sending chat messages).
    async fn websocket(stream: WebSocket, state: Arc<AxumState>) {
        // By splitting, we can send and receive at the same time.
        let (mut sender, mut receiver) = stream.split();

        let send_state = state.clone();
        let mut send_task = tokio::spawn(async move {
            let mut rx = send_state.tx.subscribe();
            let handle = send_state.app_handle.clone();
            while let Ok(msg) = rx.recv().await {
                // In any websocket error, break loop.
                let _ = handle.emit("handle-location-change", msg);
                let json = serde_json::to_string(&msg).unwrap();
                if sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        });
        let mut recv_task = tokio::spawn(async move {
            while let Some(Ok(Message::Close(_))) = receiver.next().await {
                break;
            }
        });

        let count = Self::get_and_incr(&state.client_count, 1);
        if count == 0 {
            let mut ticker = state.ticker_handle.lock().unwrap();
            let ticker_state = state.clone();

            *ticker = Some(tokio::spawn(async move {
                let app_handle = ticker_state.app_handle.clone();
                let state = app_handle.state::<AppState>();
                loop {
                    let proc_lock = state.proc.lock().await;
                    let Some(ref proc) = *proc_lock else {
                        continue;
                    };
                    let Ok(loc) = proc.get_location() else {
                        continue;
                    };
                    let _ = ticker_state.tx.send(loc);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }))
        }

        tokio::select! {
            _ = &mut send_task => recv_task.abort(),
            _ = &mut recv_task => send_task.abort(),
        };

        let count = Self::get_and_incr(&state.client_count, -1);
        if count == 1 {
            let mut ticker = state.ticker_handle.lock().unwrap();
            match *ticker {
                Some(ref t) => {
                    t.abort();
                    *ticker = None
                }
                None => {
                    log::error!("Ticker destruction failed: JoinHandle is None")
                }
            }
        }

        log::info!("client disconnected: {}", count)
    }

    fn get_and_incr(mutex: &Mutex<i32>, incr: i32) -> i32 {
        let mut m_value = mutex.lock().unwrap();
        let old = *m_value;
        *m_value += incr;
        old
    }
}

// Our shared state
struct AxumState {
    client_count: Mutex<i32>,
    // We require unique usernames. This tracks which usernames have been taken.
    app_handle: AppHandle,
    // Channel used to send messages to all connected clients.
    tx: broadcast::Sender<PlayerInfo>,
    ticker_handle: Mutex<Option<JoinHandle<()>>>,
}
