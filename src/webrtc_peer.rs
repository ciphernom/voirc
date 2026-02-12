use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_PCMU};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};
use webrtc::interceptor::registry::Registry;

use crate::state::AppState;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum WebRtcSignal {
    Offer { sdp: String },
    Answer { sdp: String },
    IceCandidate { candidate: String, sdp_mid: Option<String>, sdp_mline_index: Option<u16> },
}

pub enum InternalSignal {
    WebRtc(String, WebRtcSignal),
    Reconnect(String),
}

pub struct WebRtcPeer {
    pub nickname: String,
    peer_connection: Arc<RTCPeerConnection>,
    local_audio_track: Arc<TrackLocalStaticRTP>,
    event_tx: mpsc::UnboundedSender<InternalSignal>,
}

impl WebRtcPeer {
    pub async fn new(
        nickname: String,
        state: Arc<AppState>,
        mixer_tx: mpsc::UnboundedSender<Vec<u8>>, 
        ice_tx: mpsc::UnboundedSender<InternalSignal>, 
    ) -> Result<Self> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_PCMU.to_owned(),
                    clock_rate: 8000,
                    channels: 1,
                    sdp_fmtp_line: "".to_owned(),
                    rtcp_feedback: vec![],
                },
                payload_type: 0,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec![
                    "stun:stun.l.google.com:19302".to_owned(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);

        // Handle ICE Candidates
        let ice_tx_for_ice = ice_tx.clone();
        let nick_clone = nickname.clone();
        pc.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let ice_tx = ice_tx_for_ice.clone();
            let nick = nick_clone.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    if let Ok(json_cand) = candidate.to_json() {
                         let signal = WebRtcSignal::IceCandidate {
                             candidate: json_cand.candidate,
                             sdp_mid: json_cand.sdp_mid,
                             sdp_mline_index: json_cand.sdp_mline_index,
                         };
                         let _ = ice_tx.send(InternalSignal::WebRtc(nick, signal));
                    }
                }
            })
        }));

        // FIX 2: Handle Incoming Audio (Packet based read)
        let pc_clone = Arc::clone(&pc);
        let nick_clone = nickname.clone();
        pc_clone.on_track(Box::new(move |track, _, _| {
            let mixer_tx = mixer_tx.clone();
            let nick = nick_clone.clone();
            Box::pin(async move {
                info!("Audio track received from {}", nick);
                // Buffer is technically not needed for read() if it returns Packet, 
                // but required by trait signature.
                let mut buf = vec![0u8; 1500]; 
                loop {
                    // track.read() returns (Packet, Attributes)
                    match track.read(&mut buf).await {
                        Ok((packet, _)) => {
                            let data = packet.payload;
                            if !data.is_empty() {
                                // Send payload to mixer
                                let _ = mixer_tx.send(data.to_vec());
                            }
                        }
                        Err(e) => {
                            error!("Track end/error {}: {}", nick, e);
                            break;
                        }
                    }
                }
            })
        }));

        // Connection State Monitoring
        let nick_clone = nickname.clone();
        let state_clone = Arc::clone(&state);
        let ice_tx_clone = ice_tx.clone();
        pc.on_peer_connection_state_change(Box::new(move |s| {
            let nick = nick_clone.clone();
            let state = Arc::clone(&state_clone);
            let ice_tx = ice_tx_clone.clone();
            Box::pin(async move {
                info!("Peer {} state: {:?}", nick, s);
                match s {
                    RTCPeerConnectionState::Connected => {
                        state.update_peer_state(nick, true, false).await;
                    }
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Disconnected => {
                        state.update_peer_state(nick.clone(), false, false).await;
                        let _ = ice_tx.send(InternalSignal::Reconnect(nick));
                    }
                    _ => {}
                }
            })
        }));

        let local_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_PCMU.to_owned(),
                ..Default::default()
            },
            "audio".to_owned(),
            "voice-irc".to_owned(),
        ));

        pc.add_track(Arc::clone(&local_track) as Arc<dyn TrackLocal + Send + Sync>).await?;

        Ok(Self {
            nickname,
            peer_connection: pc,
            local_audio_track: local_track,
            event_tx: ice_tx,
        })
    }

    pub async fn create_offer(&self) -> Result<String> {
        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection.set_local_description(offer.clone()).await?;
        Ok(serde_json::to_string(&WebRtcSignal::Offer { sdp: offer.sdp })?)
    }

    pub async fn handle_offer(&self, sdp: String) -> Result<String> {
        let desc = RTCSessionDescription::offer(sdp)?;
        self.peer_connection.set_remote_description(desc).await?;
        
        let answer = self.peer_connection.create_answer(None).await?;
        self.peer_connection.set_local_description(answer.clone()).await?;

        Ok(serde_json::to_string(&WebRtcSignal::Answer { sdp: answer.sdp })?)
    }

    pub async fn handle_answer(&self, sdp: String) -> Result<()> {
        let desc = RTCSessionDescription::answer(sdp)?;
        self.peer_connection.set_remote_description(desc).await?;
        Ok(())
    }

    pub async fn add_ice_candidate(&self, candidate: String, sdp_mid: Option<String>, sdp_mline_index: Option<u16>) -> Result<()> {
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            username_fragment: None,
        };
        self.peer_connection.add_ice_candidate(init).await?;
        Ok(())
    }

    pub async fn send_audio(&self, data: &[u8]) -> Result<()> {
        self.local_audio_track.write(data).await?;
        Ok(())
    }
}
