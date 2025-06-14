use std::
    sync::{atomic::AtomicUsize, Arc}
;
use tauri::AppHandle;
use tokio::{
    sync::{broadcast, Mutex},
    task::JoinHandle,
};


#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FVector {
    x: f32,
    y: f32,
    z: f32,
}

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FRotator {
    pitch: f32,
    yaw: f32,
    roll: f32,
}

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct PlayerInfo {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FIntVector {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStorageConfig {
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub use_secure_connection: Option<bool>,
    pub auto_attach_enabled: Option<bool>,
}

impl Default for LocalStorageConfig {
    fn default() -> LocalStorageConfig {
        LocalStorageConfig {
            ip: None,
            port: None,
            use_secure_connection: None,
            auto_attach_enabled: None,
        }
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalState {
    pub proc_state: i32,
    pub server_state: i32,
    pub connection_url: Option<String>,
}

impl Default for GlobalState {
    fn default() -> GlobalState {
        GlobalState {
            proc_state: 0,
            server_state: 0,
            connection_url: None
        }
    }
}

// 클라이언트와 서버 간에 교환될 메시지의 전체 구조
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct SignalingMessage {
    pub from_id: Option<String>, // 메시지를 보낸 피어의 ID
    pub target_id: String,       // 메시지를 받을 피어의 ID ("host" 또는 클라이언트의 고유 ID)
    pub payload: Payload,
}

// 실제 WebRTC 시그널링 데이터를 담는 부분
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "kebab-case")]
pub enum Payload {
    // SDP (Session Description Protocol)
    Offer(String),
    Answer(String),
    // ICE (Interactive Connectivity Establishment)
    IceCandidate(String),
    // 세션 관리 이벤트
    NewPeer { id: String },
    PeerLeft { id: String },
}

pub struct AxumState {
    pub client_count: Arc<AtomicUsize>,
    // We require unique usernames. This tracks which usernames have been taken.
    pub app_handle: AppHandle,
    // Channel used to send messages to all connected clients.
    pub tx: broadcast::Sender<PlayerInfo>,
    pub ticker_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

#[derive(serde::Deserialize)]
pub struct WebRtcPayload {
    pub sdp: String,
}

// Answer를 보내기 위한 구조체
#[derive(serde::Serialize)]
pub struct WebRtcResponse {
    #[serde(rename = "type")]
    pub sdp_type: String,
    pub sdp: String,
}
