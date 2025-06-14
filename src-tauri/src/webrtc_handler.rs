use std::{sync::Arc, time::Duration};

use crate::{
    neoserver,
    types::{AxumState, PlayerInfo, WebRtcPayload, WebRtcResponse},
    AppState,
};
use axum::Json;
use tauri::Manager;
use tokio::sync::{broadcast, oneshot};
use tower_http::cors::CorsLayer;
use webrtc::{
    api::APIBuilder,
    data_channel::RTCDataChannel,
    ice_transport::ice_gatherer_state::RTCIceGathererState,
    peer_connection::{
        configuration::RTCConfiguration, offer_answer_options::RTCAnswerOptions,
        sdp::session_description::RTCSessionDescription,
    },
};

// --- 1. 단일 데이터 Ticker ---
pub fn spawn_data_ticker(state: Arc<AxumState>) {
    let app_handle_clone = state.app_handle.clone();
    tokio::spawn(async move {
        let app_state = app_handle_clone.state::<AppState>(); // Tauri의 AppState
        loop {
            let proc_lock = app_state.proc.lock().await;
            if let Some(ref proc) = *proc_lock {
                if let Ok(loc) = proc.get_location() {
                    // 중앙 버스로 데이터 전송
                    state.tx.send(loc).ok();
                }
            }
            drop(proc_lock);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

// --- 2. 로컬 HTTP 서버 및 핸들러 ---
pub async fn run_local_http_server(
    state: Arc<AxumState>,
    ip: String,
    port: u16,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let app = axum::Router::new()
        .route(
            "/local-webrtc-setup",
            axum::routing::post(local_webrtc_handler),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", ip, port))
        .await
        .unwrap();
    log::info!(
        "Local HTTP server for WebRTC listening on http://{}",
        listener.local_addr().unwrap()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        })
        .await
        .unwrap();
}

pub async fn local_webrtc_handler(
    axum::extract::State(state): axum::extract::State<Arc<AxumState>>,
    Json(payload): Json<WebRtcPayload>,
) -> Json<WebRtcResponse> {
    let offer = RTCSessionDescription::offer(payload.sdp).unwrap();

    let api = APIBuilder::new().build();
    let pc = Arc::new(
        api.new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap(),
    );

    let client_id = uuid::Uuid::new_v4().to_string();

    // 1. ICE 수집 완료를 기다리기 위한 채널 생성
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    pc.on_ice_gathering_state_change(Box::new(move |state| {
        if state == RTCIceGathererState::Complete {
            // 수집이 완료되면 채널로 신호를 보냄
            let _ = tx.try_send(());
        }
        Box::pin(async {})
    }));


    // 2. 받은 Offer 설정 후 Answer 생성
    pc.set_remote_description(offer).await.unwrap();
    let answer = pc
        .create_answer(Some(RTCAnswerOptions {
            voice_activity_detection: false,
        }))
        .await
        .unwrap();
    pc.set_local_description(answer).await.unwrap();

    // 3. ICE 수집이 완료될 때까지 채널에서 신호가 올 때까지 대기
    let _ = rx.recv().await;
    println!("ICE gathering complete for host.");

    // 4. 모든 정보가 담긴 완전한 Answer SDP를 가져와서 응답
    let local_desc = pc.local_description().await.unwrap();

    // 목록에 피어 저장 및 dc 설정
    neoserver::setup_peer_connection_lifecycle(state.clone(), pc.clone(), client_id.clone()).await;
    
    Json(WebRtcResponse {
        sdp_type: String::from("answer"),
        sdp: local_desc.sdp,
    })
}

// --- 3. 외부 WSS 시그널링 클라이언트 ---
pub fn spawn_external_signaling_client(
    axum_state: Arc<AxumState>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let state = axum_state.clone();
    tokio::spawn(async move { neoserver::run_webrtc_peer_logic(state, shutdown_rx) });
}
