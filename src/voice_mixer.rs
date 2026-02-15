// src/voice_mixer.rs

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use opus::{Application, Channels, Decoder, Encoder};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tracing::error;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS_COUNT: u16 = 1;
const FRAME_SIZE: usize = 960;

pub struct VoiceMixer {
    input_device: Device,
    output_device: Device,
    config: StreamConfig,
    incoming_audio: Arc<RwLock<Vec<Vec<f32>>>>,
}

/// Per-peer decoder state. Lives in the mixer task — never crosses threads.
pub struct PeerDecoders {
    decoders: HashMap<String, Decoder>,
}

impl PeerDecoders {
    pub fn new() -> Self {
        Self { decoders: HashMap::new() }
    }

    pub fn decode(&mut self, nick: &str, packet: &[u8]) -> Option<Vec<f32>> {
        let decoder = self.decoders
            .entry(nick.to_string())
            .or_insert_with(|| Decoder::new(SAMPLE_RATE, Channels::Mono).unwrap());

        let mut output = vec![0.0f32; FRAME_SIZE];
        match decoder.decode_float(packet, &mut output, false) {
            Ok(len) => {
                output.truncate(len);
                Some(output)
            }
            Err(e) => {
                error!("Opus decode error from {}: {}", nick, e);
                None
            }
        }
    }
}

impl VoiceMixer {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let input_device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("No input device"))?;
        let output_device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("No output device"))?;

        let config = StreamConfig {
            channels: CHANNELS_COUNT,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        Ok(Self {
            input_device,
            output_device,
            config,
            incoming_audio: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub fn start_input(&self, audio_tx: mpsc::UnboundedSender<Vec<u8>>) -> Result<Stream> {
        let config = self.config.clone();
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        encoder.set_inband_fec(true)?;
        encoder.set_dtx(true)?;
        let mut input_buffer: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 2);

        let stream = self.input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                input_buffer.extend_from_slice(data);
                while input_buffer.len() >= FRAME_SIZE {
                    let frame: Vec<f32> = input_buffer.drain(0..FRAME_SIZE).collect();
                    let sum_sq: f32 = frame.iter().map(|&x| x * x).sum();
                    let rms = (sum_sq / FRAME_SIZE as f32).sqrt();
                    if rms > 0.01 {
                        let mut out = [0u8; 4000];
                        match encoder.encode_float(&frame, &mut out) {
                            Ok(len) => { let _ = audio_tx.send(out[..len].to_vec()); }
                            Err(e) => error!("Opus encode error: {}", e),
                        }
                    }
                }
            },
            |err| error!("Mic error: {}", err),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn start_output(&self) -> Result<Stream> {
        let config = self.config.clone();
        let incoming = Arc::clone(&self.incoming_audio);

        let stream = self.output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
                let mut streams = incoming.blocking_write();
                if streams.is_empty() { return; }
                let gain = 1.0 / (streams.len() as f32).max(1.0).sqrt();
                for buf in streams.iter_mut() {
                    for (i, s) in data.iter_mut().enumerate() {
                        if i < buf.len() { *s += buf[i] * gain; }
                    }
                }
                let len = data.len();
                for buf in streams.iter_mut() {
                    if buf.len() >= len { buf.drain(0..len); } else { buf.clear(); }
                }
                streams.retain(|b| !b.is_empty());
            },
            |err| error!("Speaker error: {}", err),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub async fn queue_audio(&self, pcm: Vec<f32>) {
        self.incoming_audio.write().await.push(pcm);
    }
}
