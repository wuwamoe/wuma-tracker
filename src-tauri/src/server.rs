use std::{
    collections::HashMap,
    sync::{atomic::AtomicUsize, Arc},
};
use tauri::AppHandle;
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::{
    types::{AxumState, Peers},
    webrtc_handler,
};

pub struct ServerManager {
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    peers: Peers,
}

impl ServerManager {
    pub fn default() -> ServerManager {
        ServerManager {
            shutdown_tx: Option::None,
            handle: Option::None,
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start(&mut self, app_handle: AppHandle, ip: String, port: u16) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        // let (_, dummy_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);
        let peers_clone = self.peers.clone();
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
                peers: peers_clone,
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

    pub async fn shutdown(&mut self) {
        let mut peer_map = self.peers.lock().await;

        if !peer_map.is_empty() {
            // 2. 모든 피어의 .close() 비동기 작업을 수집
            let close_futures: Vec<_> = peer_map
                .drain() // HashMap을 비우면서 모든 값을 가져옴
                .map(|(_, pc)| async move { pc.close().await })
                .collect();

            log::info!("Waiting for {} peer(s) to close...", close_futures.len());

            // 3. join_all을 사용해 모든 .close() 작업이 완료될 때까지 기다림
            futures::future::join_all(close_futures).await;
            log::info!("All peer connections closed.");
        } else {
            log::info!("No active peer connections to close.");
        }

        // 4. 메인 이벤트 루프에 종료 신호를 보냄
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(()).ok();
        }

        // 5. 메인 태스크가 완전히 종료될 때까지 기다림
        if let Some(handle) = self.handle.take() {
            handle.await.ok();
        }

        log::info!("Server has been shut down gracefully.");
    }
}

// Our shared state
