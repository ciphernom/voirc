use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tracing::error;

const SAMPLE_RATE: u32 = 8000; 
const CHANNELS: u16 = 1;

pub struct VoiceMixer {
    input_device: Device,
    output_device: Device,
    config: StreamConfig,
    incoming_audio: Arc<RwLock<Vec<Vec<f32>>>>,
}

impl VoiceMixer {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let input_device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("No input device"))?;
        let output_device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("No output device"))?;

        let config = StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(160),
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
        
        let stream = self.input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let bytes: Vec<u8> = data
                    .iter()
                    .map(|&sample| {
                        let s = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                        linear_to_ulaw(s)
                    })
                    .collect();
                
                if !bytes.is_empty() {
                    let _ = audio_tx.send(bytes);
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

                let count = streams.len() as f32;
                let gain = 1.0 / count.max(1.0).sqrt();

                for stream_buf in streams.iter_mut() {
                    for (i, sample) in data.iter_mut().enumerate() {
                        if i < stream_buf.len() {
                            *sample += stream_buf[i] * gain;
                        }
                    }
                }
                
                let len = data.len();
                for stream_buf in streams.iter_mut() {
                    if stream_buf.len() >= len {
                        stream_buf.drain(0..len);
                    } else {
                        stream_buf.clear();
                    }
                }
                streams.retain(|buf| !buf.is_empty());
            },
            |err| error!("Speaker error: {}", err),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub async fn add_peer_audio(&self, packet: Vec<u8>) {
        let samples: Vec<f32> = packet
            .iter()
            .map(|&b| ulaw_to_linear(b) as f32 / 32768.0)
            .collect();

        let mut incoming = self.incoming_audio.write().await;
        incoming.push(samples);
    }
}

const BIAS: i16 = 0x84;
const CLIP: i16 = 32635;

fn linear_to_ulaw(pcm_val: i16) -> u8 {
    let mask: i16;
    let seg: i16;
    let mut pcm = pcm_val;

    if pcm < 0 {
        pcm = -pcm;
        mask = 0x7F;
    } else {
        mask = 0xFF;
    }
    if pcm > CLIP { pcm = CLIP; }
    pcm += BIAS;
    
    if pcm >= 0x4000 { seg = 7; }
    else if pcm >= 0x2000 { seg = 6; }
    else if pcm >= 0x1000 { seg = 5; }
    else if pcm >= 0x0800 { seg = 4; }
    else if pcm >= 0x0400 { seg = 3; }
    else if pcm >= 0x0200 { seg = 2; }
    else if pcm >= 0x0100 { seg = 1; }
    else { seg = 0; }

    let aval = (seg << 4) | ((pcm >> (seg + 3)) & 0xF);
    (aval as u8) ^ (mask as u8)
}

fn ulaw_to_linear(u_val: u8) -> i16 {
    let mut t: i16;
    let u_val = !u_val; 
    t = ((u_val & 0xF) as i16) << 3;
    t += BIAS;
    t <<= (u_val & 0x70) >> 4;
    t -= BIAS;
    if (u_val & 0x80) == 0 { t } else { -t }
}
