use std::sync::{atomic::AtomicUsize, Arc};
use tauri::AppHandle;
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::{types::AxumState, webrtc_handler};

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
        // let (_, dummy_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        self.handle = Some(tokio::spawn(async move {
            // Set up application state for use with with_state().
            let (tx, _rx) = broadcast::channel(100);
            let client_count = Arc::new(AtomicUsize::new(0));
            let ticker_handle = Arc::new(Mutex::new(None));

            let app = app_handle.clone();
            let axum_state = Arc::new(AxumState {
                client_count,
                app_handle: app,
                tx,
                ticker_handle,
            });

            // 3. 단일 데이터 Ticker 실행
            webrtc_handler::spawn_data_ticker(axum_state.clone());

            // 4. 외부 네트워크 연결을 위한 시그널링 클라이언트 실행
            // webrtc_handler::spawn_external_signaling_client(axum_state.clone(), dummy_rx);

            // 5. 로컬 네트워크 연결을 위한 Axum HTTP 서버 실행
            webrtc_handler::run_local_http_server(axum_state, ip, port, shutdown_rx).await;
        }));
    }

    pub async fn restart(&mut self, app_handle: AppHandle, ip: String, port: u16) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            self.handle.take().unwrap().await.ok();
        }
        self.start(app_handle, ip, port).await;
    }

    // async fn websocket_handler(
    //     ws: WebSocketUpgrade,
    //     State(state): State<Arc<AxumState>>,
    // ) -> impl IntoResponse {
    //     log::info!("client connected");
    //     ws.on_upgrade(|socket| Self::websocket(socket, state))
    // }

    // // This function deals with a single websocket connection, i.e., a single
    // // connected client / user, for which we will spawn two independent tasks (for
    // // receiving / sending chat messages).
    // async fn websocket(stream: WebSocket, state: Arc<AxumState>) {
    //     // By splitting, we can send and receive at the same time.
    //     let (mut sender, mut receiver) = stream.split();

    //     let send_state = state.clone();
    //     let mut send_task = tokio::spawn(async move {
    //         let mut rx = send_state.tx.subscribe();
    //         let handle = send_state.app_handle.clone();
    //         while let Ok(msg) = rx.recv().await {
    //             // In any websocket error, break loop.
    //             let _ = handle.emit("handle-location-change", msg);
    //             let json = serde_json::to_string(&msg).unwrap();
    //             if sender.send(Message::Text(json.into())).await.is_err() {
    //                 break;
    //             }
    //         }
    //     });
    //     let mut recv_task = tokio::spawn(async move {
    //         while let Some(Ok(Message::Close(_))) = receiver.next().await {
    //             break;
    //         }
    //     });

    //     let count = Self::get_and_incr(&state.client_count, 1);
    //     if count == 0 {
    //         let mut ticker = state.ticker_handle.lock().unwrap();
    //         let ticker_state = state.clone();
    //         let handle = ticker_state.app_handle.clone();

    //         *ticker = Some(tokio::spawn(async move {
    //             let app_handle = ticker_state.app_handle.clone();
    //             let state = app_handle.state::<AppState>();
    //             loop {
    //                 let proc_lock = state.proc.lock().await;
    //                 let Some(ref proc) = *proc_lock else {
    //                     continue;
    //                 };
    //                 match proc.get_location() {
    //                     Ok(loc) => {
    //                         let _ = ticker_state.tx.send(loc);
    //                         tokio::time::sleep(Duration::from_millis(500)).await;
    //                     }
    //                     Err(e) => {
    //                         let _ = handle.emit("tracker-error", e);
    //                         tokio::time::sleep(Duration::from_millis(500)).await;
    //                     }
    //                 }
    //             }
    //         }))
    //     }

    //     tokio::select! {
    //         _ = &mut send_task => recv_task.abort(),
    //         _ = &mut recv_task => send_task.abort(),
    //     };

    //     let count = Self::get_and_incr(&state.client_count, -1);
    //     if count == 1 {
    //         let mut ticker = state.ticker_handle.lock().unwrap();
    //         match *ticker {
    //             Some(ref t) => {
    //                 t.abort();
    //                 *ticker = None
    //             }
    //             None => {
    //                 log::error!("Ticker destruction failed: JoinHandle is None")
    //             }
    //         }
    //     }

    //     log::info!("client disconnected: {}", count)
    // }

    // fn get_and_incr(mutex: &Mutex<i32>, incr: i32) -> i32 {
    //     let mut m_value = mutex.lock().unwrap();
    //     let old = *m_value;
    //     *m_value += incr;
    //     old
    // }
}

// Our shared state
