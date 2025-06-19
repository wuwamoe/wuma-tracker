use crate::types::{Peer, PlayerInfo, RtcSignal, SignalPacket, SERVER_ID};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::peer_connection::configuration::RTCConfiguration;

pub(crate) struct PeerManager {
    peers: HashMap<String, Peer>,
    pm_sh_tx: mpsc::Sender<SignalPacket>,
}

impl PeerManager {
    pub fn new(pm_sh_tx: mpsc::Sender<SignalPacket>) -> Self {
        Self {
            peers: HashMap::new(),
            pm_sh_tx,
        }
    }

    pub async fn handle_signaling_message(&mut self, message: SignalPacket) -> Result<()> {
        let client_id = message.from;
        // 메시지를 처리할 대상 Peer를 찾습니다.
        let peer = self.peers.get(&client_id).context(format!(
            "Received signal for non-existent peer: {}",
            client_id
        ))?;

        match message.msg {
            RtcSignal::Answer(answer) => {
                log::info!("[{}] Received Answer.", client_id);
                // 클라이언트가 보낸 Answer를 RemoteDescription으로 설정합니다.
                peer.connection.set_remote_description(answer).await?;
            }
            RtcSignal::IceCandidate(candidate) => {
                log::info!(
                    "[{}] Received ICE Candidate: {}",
                    client_id,
                    candidate.candidate
                );
                // 클라이언트가 보낸 ICE Candidate를 추가합니다.
                peer.connection.add_ice_candidate(candidate).await?;
            }
            // Offer 등 서버가 클라이언트로부터 받아서는 안 되는 메시지 유형입니다.
            signal => {
                log::warn!(
                    "[{}] Received unexpected signal from client: {:?}",
                    client_id,
                    signal
                );
            }
        }
        Ok(())
    }

    pub async fn handle_new_client(&mut self, client_id: String) -> Result<()> {
        let api = APIBuilder::new().build();
        let config = RTCConfiguration::default();
        let pc = Arc::new(api.new_peer_connection(config).await?);

        // 1. ICE Candidate 생성을 감지하는 핸들러를 등록합니다.
        // PeerManager의 송신 채널(pm_sh_tx)을 클론하여 핸들러 내부에서 사용합니다.
        let pm_sh_tx = self.pm_sh_tx.clone();
        let client_id_clone = client_id.clone();
        pc.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            // pm_sh_tx와 client_id의 소유권을 핸들러로 이동시킵니다.
            let pm_sh_tx = pm_sh_tx.clone();
            let client_id = client_id_clone.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    match candidate.to_json() {
                        // to_json은 async 함수
                        Ok(candidate_init) => {
                            let signal = SignalPacket {
                                from: "SERVER_ID".to_string(),
                                to: client_id.clone(),
                                msg: RtcSignal::IceCandidate(candidate_init),
                            };
                            if let Err(e) = pm_sh_tx.send(signal).await {
                                log::error!("[{}] Failed to send ICE candidate: {}", client_id, e);
                            }
                        }
                        Err(e) => {
                            log::error!("[{}] Failed to serialize ICE candidate: {}", client_id, e);
                        }
                    }
                }
            })
        }));

        let dc = pc.create_data_channel("data", None).await?;
        let new_peer = Peer {
            connection: pc.clone(),
            data_channel: dc,
        };
        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer.clone()).await?;

        self.pm_sh_tx
            .send(SignalPacket {
                from: SERVER_ID.to_string(),
                to: client_id.clone(),
                msg: RtcSignal::Offer(offer),
            })
            .await?;

        self.peers.insert(client_id, new_peer);

        Ok(())
    }

    pub async fn handle_client_disconnect(&mut self, client_id: String) -> Result<()> {
        if let Some(peer) = self.peers.remove(&client_id) {
            // PeerConnection을 정상적으로 종료하여 관련 리소스를 모두 해제합니다.
            if let Err(e) = peer.connection.close().await {
                // 종료 중 에러가 발생하더라도, 이미 맵에서 제거되었으므로 경고만 기록합니다.
                log::warn!("[{}] Error while closing peer connection: {}", client_id, e);
            } else {
                log::info!(
                    "[{}] Peer connection closed and resources released.",
                    client_id
                );
            }
        } else {
            // 이미 제거되었거나 존재하지 않는 클라이언트일 수 있습니다.
            log::warn!("[{}] Tried to disconnect a non-existent peer.", client_id);
        }
        Ok(())
    }

    pub async fn broadcast_data(&self, message: &PlayerInfo) -> Result<()> {
        let payload = serde_json::to_string(message)
            .context("DataChannel send error: could not serialize data")?;

        for (client_id, peer) in &self.peers {
            if peer.data_channel.ready_state() == RTCDataChannelState::Open {
                if let Err(e) = peer.data_channel.send_text(&payload).await {
                    log::warn!(
                        "[{}] DataChannel send error, but continuing broadcast: {}",
                        client_id,
                        e
                    );
                }
            }
        }
        Ok(())
    }

    // Supervisor가 피어의 수를 확인할 수 있는 메서드를 추가
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }
}
