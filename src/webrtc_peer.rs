use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{error, info};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

use crate::config::{ConnState, TurnServer};
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
    ConnFailed(String),
}

pub struct ReceivedFile {
    pub from: String,
    pub name: String,
    pub data: Vec<u8>,
}

struct InFlightFile {
    name: String,
    data: Vec<u8>,
}

pub struct WebRtcPeer {
    pub nickname: String,
    peer_connection: Arc<RTCPeerConnection>,
    local_audio_track: Arc<TrackLocalStaticRTP>,
    data_channel: Arc<RwLock<Option<Arc<RTCDataChannel>>>>,
    file_tx: mpsc::UnboundedSender<ReceivedFile>,
}

fn setup_dc_receive(
    dc: &Arc<RTCDataChannel>,
    nick: String,
    file_tx: mpsc::UnboundedSender<ReceivedFile>,
) {
    let in_flight: Arc<Mutex<Option<InFlightFile>>> = Arc::new(Mutex::new(None));

    let nick_c = nick.clone();
    let file_tx_c = file_tx.clone();
    let in_flight_c = in_flight.clone();

    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let nick = nick_c.clone();
        let file_tx = file_tx_c.clone();
        let in_flight = in_flight_c.clone();
        Box::pin(async move {
            if msg.is_string {
                let text = String::from_utf8_lossy(&msg.data);
                if let Some(rest) = text.strip_prefix("FILE:") {
                    if let Some((name, size_str)) = rest.rsplit_once(':') {
                        let size = size_str.parse::<usize>().unwrap_or(0);
                        *in_flight.lock().await = Some(InFlightFile {
                            name: name.to_string(),
                            data: Vec::with_capacity(size),
                        });
                    }
                } else if text.trim() == "FILE_END" {
                    if let Some(file) = in_flight.lock().await.take() {
                        let _ = file_tx.send(ReceivedFile {
                            from: nick,
                            name: file.name,
                            data: file.data,
                        });
                    }
                }
            } else {
                let mut guard = in_flight.lock().await;
                if let Some(ref mut file) = *guard {
                    file.data.extend_from_slice(&msg.data);
                }
            }
        })
    }));
}

fn build_ice_servers(turn_servers: &[TurnServer]) -> Vec<RTCIceServer> {
    let mut servers = vec![
        RTCIceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".to_owned(),
                "stun:stun1.l.google.com:19302".to_owned(),
            ],
            ..Default::default()
        },
    ];

    for ts in turn_servers {
        servers.push(RTCIceServer {
            urls: vec![ts.url.clone()],
            username: ts.username.clone(),
            credential: ts.credential.clone(),
            ..Default::default()
        });
    }

    servers
}

impl WebRtcPeer {
    pub async fn new(
        nickname: String,
        state: Arc<AppState>,
        mixer_tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
        ice_tx: mpsc::UnboundedSender<InternalSignal>,
        file_tx: mpsc::UnboundedSender<ReceivedFile>,
        turn_servers: Vec<TurnServer>,
    ) -> Result<Self> {
        let mut media_engine = MediaEngine::default();

        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_OPUS.to_owned(),
                    clock_rate: 48000,
                    channels: 1,
                    sdp_fmtp_line: "".to_owned(),
                    rtcp_feedback: vec![],
                },
                payload_type: 111,
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
            ice_servers: build_ice_servers(&turn_servers),
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);

        // Track connection start time for timeout detection
        state.set_peer_connecting(&nickname).await;

        // ICE candidates
        let ice_tx_c = ice_tx.clone();
        let nick_c = nickname.clone();
        pc.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let tx = ice_tx_c.clone();
            let nick = nick_c.clone();
            Box::pin(async move {
                if let Some(c) = candidate {
                    if let Ok(j) = c.to_json() {
                        let _ = tx.send(InternalSignal::WebRtc(nick, WebRtcSignal::IceCandidate {
                            candidate: j.candidate,
                            sdp_mid: j.sdp_mid,
                            sdp_mline_index: j.sdp_mline_index,
                        }));
                    }
                }
            })
        }));

        // Incoming audio
        let nick_c = nickname.clone();
        pc.on_track(Box::new(move |track, _, _| {
            let tx = mixer_tx.clone();
            let nick = nick_c.clone();
            Box::pin(async move {
                info!("Audio track received from {}", nick);
                let mut buf = vec![0u8; 1500];
                loop {
                    match track.read(&mut buf).await {
                        Ok((pkt, _)) => {
                            if !pkt.payload.is_empty() {
                                let _ = tx.send((nick.clone(), pkt.payload.to_vec()));
                            }
                        }
                        Err(e) => {
                            error!("Track end {}: {}", nick, e);
                            break;
                        }
                    }
                }
            })
        }));

        // Connection state
        let nick_c = nickname.clone();
        let state_c = Arc::clone(&state);
        let ice_tx_c = ice_tx.clone();
        pc.on_peer_connection_state_change(Box::new(move |s| {
            let nick = nick_c.clone();
            let state = Arc::clone(&state_c);
            let tx = ice_tx_c.clone();
            Box::pin(async move {
                info!("Peer {} state: {:?}", nick, s);
                match s {
                    RTCPeerConnectionState::Connected => {
                        state.update_peer_state(nick.clone(), true, false).await;
                        state.set_peer_conn_state(&nick, ConnState::Connected).await;
                    }
                    RTCPeerConnectionState::Failed => {
                        state.update_peer_state(nick.clone(), false, false).await;
                        state.set_peer_conn_state(&nick, ConnState::Failed).await;
                        let _ = tx.send(InternalSignal::ConnFailed(nick.clone()));
                        let _ = tx.send(InternalSignal::Reconnect(nick));
                    }
                    RTCPeerConnectionState::Disconnected => {
                        state.update_peer_state(nick.clone(), false, false).await;
                        let _ = tx.send(InternalSignal::Reconnect(nick));
                    }
                    _ => {}
                }
            })
        }));

        // Data channel holder
        let dc_holder: Arc<RwLock<Option<Arc<RTCDataChannel>>>> = Arc::new(RwLock::new(None));

        let dc_h = dc_holder.clone();
        let nick_c = nickname.clone();
        let ftx = file_tx.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let holder = dc_h.clone();
            let nick = nick_c.clone();
            let tx = ftx.clone();
            Box::pin(async move {
                info!("Received data channel from {}", nick);
                setup_dc_receive(&dc, nick, tx);
                *holder.write().await = Some(dc);
            })
        }));

        // Local audio track
        let local_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 1,
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
            data_channel: dc_holder,
            file_tx,
        })
    }

    pub async fn create_offer(&self) -> Result<String> {
        let dc = self.peer_connection.create_data_channel("files", None).await?;
        setup_dc_receive(&dc, self.nickname.clone(), self.file_tx.clone());
        *self.data_channel.write().await = Some(dc);

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
        self.peer_connection.add_ice_candidate(RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            username_fragment: None,
        }).await?;
        Ok(())
    }

    pub async fn send_audio(&self, data: &[u8]) -> Result<()> {
        self.local_audio_track.write(data).await?;
        Ok(())
    }

    pub async fn send_file(&self, name: &str, data: &[u8]) -> Result<()> {
        let dc = self.data_channel.read().await;
        let dc = dc.as_ref().ok_or_else(|| anyhow::anyhow!("Data channel not ready"))?;

        dc.send_text(format!("FILE:{}:{}", name, data.len())).await?;

        for chunk in data.chunks(16384) {
            dc.send(&bytes::Bytes::copy_from_slice(chunk)).await?;
        }

        dc.send_text("FILE_END".to_string()).await?;
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.peer_connection.close().await;
    }
}
