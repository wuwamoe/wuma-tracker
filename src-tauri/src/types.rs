use anyhow::Result;
use futures::channel::mpsc as futures_mpsc;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStorageConfig {
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub use_secure_connection: Option<bool>,
    pub auto_attach_enabled: Option<bool>,
    pub start_in_tray: Option<bool>,
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
    Data(PlayerInfo)
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct SignalPacket {
    pub from: String,
    pub to: String,
    pub msg: RtcSignal,
}

#[derive(Error, Debug)]
pub enum NativeError {
    #[error("프로세스가 종료되었습니다.")]
    ProcessTerminated,

    #[error("포인터 체인 탐색 실패: {message}")]
    PointerChainError { message: String },

    #[error("값 읽기 실패: {message}")]
    ValueReadError { message: String },
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
    Local
}

#[derive(Debug)]
pub enum WsRouteInfo {
    External,
    Local(mpsc::Sender<String>),
}

#[derive(Debug)]
pub struct ExternalSession {
    // command_processor가 사용할 메시지 발신용 채널의 Sender
    pub ws_sender: futures_mpsc::UnboundedSender<TungsteniteMessage>,
    // 이 세션의 모든 태스크를 한 번에 종료시키기 위한 핸들
    pub shutdown_handle: JoinHandle<()>,
}

impl Default for LocalStorageConfig {
    fn default() -> LocalStorageConfig {
        LocalStorageConfig {
            ip: None,
            port: None,
            use_secure_connection: None,
            auto_attach_enabled: None,
            start_in_tray: None,
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
