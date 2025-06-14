use anyhow::Result;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use std::sync::Arc;
use std::{collections::HashMap, time::Duration};
use tauri::Manager;
use tokio::sync::{broadcast, Mutex};
use tokio::{net::TcpStream, sync::oneshot};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::types::{AxumState, Peers, PlayerInfo, WsSender};
use crate::{
    types::{Payload, SignalingMessage},
    AppState,
};

pub async fn run_webrtc_peer_logic(state: Arc<AxumState>, mut shutdown_rx: oneshot::Receiver<()>) {
    // 1. 룸 코드 생성 및 UI로 전송
    let room_code = "foo-bar";
    // app_handle.emit("webrtc-room-code", &room_code).unwrap();
    log::info!("Room Code Created: {}", room_code);

    // 2. Cloudflare 시그널링 서버 접속
    let url = format!("wss://your-worker.dev/{}", room_code);
    let (ws_stream, _) = match connect_async(&url).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to connect to signaling server: {}", e);
            return;
        }
    };
    let (ws_sender, mut ws_receiver) = ws_stream.split();
    let ws_sender = Arc::new(Mutex::new(ws_sender));
    log::info!("Connected to signaling server.");

    // 3. 다수의 PeerConnection을 관리하기 위한 HashMap 생성
    let peers: Peers = Arc::new(Mutex::new(HashMap::new()));
    let (tx, _) = broadcast::channel::<PlayerInfo>(100);

    // 티커
    let ticker_tx = tx.clone();
    let app_handle_clone = state.app_handle.clone();
    tokio::spawn(async move {
        let state = app_handle_clone.state::<AppState>();
        loop {
            let proc_lock = state.proc.lock().await;
            if let Some(ref proc) = *proc_lock {
                if let Ok(loc) = proc.get_location() {
                    // 데이터를 broadcast 채널로 보냄
                    if ticker_tx.send(loc).is_err() {
                        // 수신자가 아무도 없으면 에러가 발생하지만, 정상적인 상황임
                    }
                }
            }
            drop(proc_lock);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // 4. 메인 이벤트 루프: 시그널 서버 메시지 처리 및 앱 종료 감지
    loop {
        tokio::select! {
            // 시그널 서버로부터 메시지 수신
            Some(Ok(msg)) = ws_receiver.next() => {
                if let WsMessage::Text(text) = msg {
                    match serde_json::from_str::<SignalingMessage>(&text) {
                        Ok(sig_msg) => {
                            // Peer 관리를 위한 비동기 태스크 실행
                            handle_signaling_message(
                                sig_msg,
                                Arc::clone(&peers),
                                Arc::clone(&ws_sender),
                                tx.clone(),
                                state.clone()
                            ).await;
                        }
                        Err(e) => log::error!("Failed to parse signaling message: {}", e),
                    }
                }
            }
            // Tauri 앱 종료 신호 수신
            _ = &mut shutdown_rx => {
                log::info!("Gracefully shutting down WebRTC peers...");
                let mut peer_map = peers.lock().await;
                for (_, pc) in peer_map.drain() {
                    pc.close().await.ok();
                }
                break;
            }
        }
    }
}

/// 수신된 시그널링 메시지를 종류에 따라 처리하는 함수
async fn handle_signaling_message(
    msg: SignalingMessage,
    peers: Peers,
    ws_sender: WsSender,
    tx: broadcast::Sender<PlayerInfo>,
    state: Arc<AxumState>,
) {
    // `from_id`가 없는 메시지는 처리하지 않음 (NewPeer 제외)
    let client_id = match msg.payload {
        Payload::NewPeer { ref id } => id.clone(),
        _ => msg.from_id.unwrap_or_default(),
    };
    if client_id.is_empty() {
        return;
    }

    match msg.payload {
        // 새로운 클라이언트가 방에 접속했을 때
        Payload::NewPeer { id } => {
            log::info!(
                "Peer '{}' has joined the room. Creating new PeerConnection.",
                id
            );

            // 새로운 Peer를 위한 WebRTC 연결 생성 시작
            if let Err(e) = create_new_peer(id, peers, ws_sender, tx, state).await {
                log::error!("Failed to create new peer: {}", e);
            }
        }
        // 클라이언트로부터 Answer를 받았을 때
        Payload::Answer(sdp) => {
            if let Some(pc) = peers.lock().await.get(&client_id) {
                let answer = RTCSessionDescription::answer(sdp).unwrap();
                if let Err(e) = pc.set_remote_description(answer).await {
                    log::error!("Failed to set remote description for {}: {}", client_id, e);
                }
            }
        }
        // 클라이언트로부터 ICE Candidate를 받았을 때
        Payload::IceCandidate(candidate) => {
            if let Some(pc) = peers.lock().await.get(&client_id) {
                let cand: RTCIceCandidateInit = serde_json::from_str(&candidate).unwrap();
                if let Err(e) = pc.add_ice_candidate(cand).await {
                    log::error!("Failed to add ice candidate for {}: {}", client_id, e);
                }
            }
        }
        // 클라이언트가 방을 나갔을 때
        Payload::PeerLeft { id } => {
            log::info!("Peer '{}' has left the room.", id);
            if let Some(pc) = peers.lock().await.remove(&id) {
                pc.close().await.ok();
            }
        }
        _ => {}
    }
}

/// 새로운 클라이언트를 위해 PeerConnection을 생성하고 Offer를 보내는 함수
async fn create_new_peer(
    client_id: String,
    peers: Peers,
    ws_sender: WsSender,
    tx: broadcast::Sender<PlayerInfo>,
    state: Arc<AxumState>,
) -> Result<(), anyhow::Error> {
    // 1. PeerConnection 생성
    let api = APIBuilder::new().build();
    let config = RTCConfiguration::default();
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);

    // 2. 데이터 채널 생성 및 데이터 전송 로직 연결
    let data_channel_init = RTCDataChannelInit {
        ordered: Some(true),
        ..Default::default()
    };
    let data_channel = peer_connection
        .create_data_channel("data", Some(data_channel_init))
        .await?;
    let dc_clone = data_channel.clone();
    let client_id_clone = client_id.clone();
    setup_data_channel_lifecycle(dc_clone, state.clone(), client_id_clone);

    // 3. ICE Candidate가 생성될 때마다 시그널 서버로 전송
    let client_id_clone = client_id.clone();
    let ws_sender_clone = ws_sender.clone();
    peer_connection.on_ice_candidate(Box::new(move |candidate| {
        let ws_sender_clone = Arc::clone(&ws_sender_clone);
        let client_id_clone = client_id_clone.clone();

        Box::pin(async move {
            if let Some(c) = candidate {
                let cand_str = match c.to_json() {
                    Ok(s) => s.candidate,
                    Err(e) => {
                        log::error!(
                            "[{}] Failed to serialize ICE candidate: {}",
                            client_id_clone,
                            e
                        );
                        return;
                    }
                };
                let msg = SignalingMessage {
                    from_id: Some("host".to_string()),
                    target_id: client_id_clone.clone(),
                    payload: Payload::IceCandidate(cand_str),
                };
                let json_msg = serde_json::to_string(&msg).unwrap();
                ws_sender_clone
                    .lock()
                    .await
                    .send(WsMessage::Text(json_msg.into()))
                    .await
                    .ok();
            }
        })
    }));

    // 4. 생성된 PeerConnection을 HashMap에 저장
    peers
        .lock()
        .await
        .insert(client_id.clone(), Arc::clone(&peer_connection));

    // 5. Offer를 생성하여 해당 클라이언트에게만 전송
    let offer = peer_connection.create_offer(None).await?;
    peer_connection.set_local_description(offer.clone()).await?;

    let msg = SignalingMessage {
        from_id: Some("host".to_string()),
        target_id: client_id,
        payload: Payload::Offer(offer.sdp),
    };
    let json_msg = serde_json::to_string(&msg).unwrap();
    ws_sender
        .lock()
        .await
        .send(WsMessage::Text(json_msg.into()))
        .await?;

    Ok(())
}

/// 필요할 경우 Ticker 태스크를 생성하는 함수
fn start_ticker_if_needed(state: Arc<AxumState>) {
    let state_clone = state.clone();
    tokio::spawn(async move {
        let mut ticker_handle = state_clone.ticker_handle.lock().await;
        // 이미 실행 중인 Ticker가 없는지 확인
        if ticker_handle.is_none() {
            let data_tx = state_clone.tx.clone();
            let app_handle = state_clone.app_handle.clone();

            let handle = tokio::spawn(async move {
                let app_state = app_handle.state::<crate::AppState>();
                loop {
                    let proc_lock = app_state.proc.lock().await;
                    if let Some(ref proc) = *proc_lock {
                        if let Ok(loc) = proc.get_location() {
                            data_tx.send(loc).ok();
                        }
                    }
                    drop(proc_lock);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
            // 생성된 태스크의 핸들을 저장
            *ticker_handle = Some(handle);
        }
    });
}

/// 필요할 경우 Ticker 태스크를 중단하는 함수
fn stop_ticker_if_needed(state: Arc<AxumState>) {
    tokio::spawn(async move {
        let mut ticker_handle = state.ticker_handle.lock().await;
        // 실행 중인 Ticker 핸들을 가져와서 abort() 호출
        if let Some(handle) = ticker_handle.take() {
            handle.abort();
            log::info!("Ticker task aborted.");
        }
    });
}

pub async fn setup_peer_connection_lifecycle(
    state: Arc<AxumState>,
    pc: Arc<RTCPeerConnection>,
    client_id: String,
) {
    let client_id_clone = client_id.clone();
    let peers_clone = state.peers.clone();
    pc.on_peer_connection_state_change(Box::new(move |connection_state: RTCPeerConnectionState| {
        log::info!(
            "Connection State for peer '{}' has changed: {}",
            client_id_clone,
            connection_state
        );

        // 연결이 끊기거나 실패하면, 공유 상태의 피어 목록에서 자신을 제거합니다.
        if connection_state == RTCPeerConnectionState::Failed
            || connection_state == RTCPeerConnectionState::Closed
            || connection_state == RTCPeerConnectionState::Disconnected
        {
            let peers = Arc::clone(&peers_clone);
            let client_id_clone2 = client_id_clone.clone();
            tokio::spawn(async move {
                log::info!("Removing peer '{}' from state.", client_id_clone2);
                peers.lock().await.remove(&client_id_clone2);
            });
        }
        Box::pin(async {})
    }));

    let state_clone = state.clone();
    let client_id_clone = client_id.clone();
    pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        log::info!("✅ New DataChannel '{}' from client.", dc.label());

        let state_clone_dc = state_clone.clone();
        let dc_clone = Arc::clone(&dc);
        let client_id_dc = client_id_clone.clone();
        dc.on_open(Box::new(move || {
            log::info!("✅ Data channel '{}' open.", dc_clone.label());
            setup_data_channel_lifecycle(
                dc_clone,
                state_clone_dc,
                String::from(client_id_dc),
            );
            Box::pin(async {})
        }));

        Box::pin(async {})
    }));

    state.peers.lock().await.insert(client_id, pc.clone());
}

pub fn setup_data_channel_lifecycle(
    dc: Arc<RTCDataChannel>,
    state: Arc<AxumState>,
    client_id: String,
) {
    // on_open: 클라이언트가 1명이 되는 순간 Ticker 시작
    let state_clone_on_open = state.clone();
    let dc_clone = dc.clone();
    dc.on_open(Box::new(move || {
        let state = state_clone_on_open.clone();

        // 이전 클라이언트 수를 가져오고, 1 증가시킴
        let old_count = state
            .client_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // 이전 카운트가 0이었다면 (즉, 지금 1명이 되었다면) Ticker를 시작
        if old_count == 0 {
            log::info!("First client connected. Starting data ticker...");
            // 별도 함수로 분리하여 Ticker 시작 로직 실행
            start_ticker_if_needed(state.clone());
        }

        // 중앙 버스 구독 및 데이터 전송 태스크 실행 (이전 답변과 동일)
        let mut data_rx = state.tx.subscribe();
        let client_id_num = format!("{}_{}", client_id, old_count + 1);
        tokio::spawn(async move {
            while let Ok(data) = data_rx.recv().await {
                if dc_clone
                    .send_text(serde_json::to_string(&data).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            log::info!("[{}] Data sender task has stopped.", client_id_num);
        });

        Box::pin(async {})
    }));

    // on_close: 클라이언트가 0명이 되는 순간 Ticker 중단
    let state_clone_on_close = Arc::clone(&state);
    dc.on_close(Box::new(move || {
        let state = Arc::clone(&state_clone_on_close);

        // 클라이언트 수를 1 감소시키고, 이전 값을 가져옴
        let old_count = state
            .client_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

        // 이전 카운트가 1이었다면 (즉, 지금 0명이 되었다면) Ticker를 중단
        if old_count == 1 {
            log::info!("Last client disconnected. Stopping data ticker...");
            // 별도 함수로 분리하여 Ticker 중단 로직 실행
            stop_ticker_if_needed(Arc::clone(&state));
        }

        Box::pin(async {})
    }));
}
