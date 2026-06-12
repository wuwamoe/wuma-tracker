use anyhow::Result;
use futures::channel::mpsc as futures_mpsc;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize, serde::Deserialize, Debug)]
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

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FTransformDouble {
    pub rot_x: f32,
    pub rot_y: f32,
    pub rot_z: f32,
    pub rot_w: f32,
    pub loc_x: f32,
    pub loc_y: f32,
    pub loc_z: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub scale_z: f32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStorageConfig {
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub use_secure_connection: Option<bool>,
    pub auto_attach_enabled: Option<bool>,
    pub start_in_tray: Option<bool>,
    pub game_path: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "kebab-case")]
pub enum RtcSignal {
    Answer(RTCSessionDescription),
    Offer(RTCSessionDescription),
    IceCandidate(RTCIceCandidateInit),
    NewPeer,
    PeerLeft,
    NewLocalPeer,
    LocalOffer,
    Data(PlayerInfo),
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct SignalPacket {
    pub from: String,
    pub to: String,
    pub msg: RtcSignal,
}

#[derive(Error, Debug)]
pub enum NativeError {
    #[error("proc_terminated")]
    ProcessTerminated,

    #[error("{message}")]
    PointerChainError { message: String },

    #[error("{message}")]
    ValueReadError { message: String },
}

impl NativeError {
    /// 프론트엔드 표시용 간결한 한국어 메시지
    pub fn user_message(&self) -> &'static str {
        match self {
            NativeError::ProcessTerminated => "게임 프로세스가 종료되었습니다.",
            NativeError::PointerChainError { message }
            | NativeError::ValueReadError { message } => {
                if message.contains("[ACCESS]") {
                    "메모리 접근 거부 (ACE 핸들 권한 박탈)"
                } else if message.contains("[INV_HDL]") {
                    "프로세스 핸들이 무효화되었습니다."
                } else if message.contains("[PARTIAL]") {
                    "ACE 메모리 보호 중 (게임 초기화/로딩 중...)"
                } else if message.contains("NULL") || message.contains("addr=0") {
                    "게임 월드 초기화 중 (잠시 대기...)"
                } else {
                    "메모리 읽기 실패"
                }
            }
        }
    }
}

pub enum CollectorMessage {
    Data(PlayerInfo),
    TemporalError(String),
    Terminated,
    OffsetFound(String),
}

#[derive(Debug)]
pub enum SupervisorCommand {
    AttachProcess(String, oneshot::Sender<Result<(), String>>),
    LaunchAndAttach(String, oneshot::Sender<Result<(), String>>),
    RestartSignalingServer,
    RestartExternalConnection(oneshot::Sender<Result<String, String>>),
}

// 한 명의 클라이언트에 대한 모든 WebRTC 관련 리소스를 묶는 구조체
pub struct Peer {
    pub connection: Arc<RTCPeerConnection>,
    pub data_channel: Arc<RTCDataChannel>,
}

pub enum ManagedPeer {
    External(Peer),
    Local,
}

#[derive(Debug)]
pub enum WsRouteInfo {
    External,
    Local(mpsc::Sender<RtcSignal>),
}

#[derive(Debug)]
pub struct ExternalSession {
    pub ws_sender: futures_mpsc::UnboundedSender<TungsteniteMessage>,
    pub shutdown_handle: JoinHandle<()>,
    /// 의도적 종료 시 cancel() 호출 → 자동 재연결 방지
    pub cancel: CancellationToken,
}

impl Default for LocalStorageConfig {
    fn default() -> LocalStorageConfig {
        LocalStorageConfig {
            ip: None,
            port: None,
            use_secure_connection: None,
            auto_attach_enabled: None,
            start_in_tray: None,
            game_path: None,
        }
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalState {
    pub proc_state: i32,
    pub server_state: i32,
    pub connection_url: Option<String>,
    pub external_connection_code: Option<String>,
    pub active_offset_name: Option<String>,
}

impl Default for GlobalState {
    fn default() -> GlobalState {
        GlobalState {
            proc_state: 0,
            server_state: 0,
            connection_url: None,
            external_connection_code: None,
            active_offset_name: None,
        }
    }
}

pub const SERVER_ID: &str = "SERVER";
