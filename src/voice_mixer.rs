// src/voice_mixer.rs

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use opus::{Application, Channels, Decoder, Encoder}; // Import Opus
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tracing::error;

const SAMPLE_RATE: u32 = 48000; // Standard for Opus
const CHANNELS_COUNT: u16 = 1;  // Mono input/output for simplicity
const FRAME_SIZE: usize = 960;  // 20ms at 48kHz

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
            channels: CHANNELS_COUNT,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default, // Let CPAL decide, we will buffer manually
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
        
        // Initialize Opus Encoder
        // Application::Voip optimizes for voice
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        encoder.set_inband_fec(true)?; // Forward Error Correction
        encoder.set_dtx(true)?;        // Discontinuous Transmission (Internal VAD)
        
        let mut input_buffer: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 2);

        let stream = self.input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // 1. Buffer incoming samples
                input_buffer.extend_from_slice(data);

                // 2. Process fixed-size frames (20ms)
                while input_buffer.len() >= FRAME_SIZE {
                    let frame: Vec<f32> = input_buffer.drain(0..FRAME_SIZE).collect();
                    
                    // 3. Manual VAD (Noise Gate)
                    // Calculate RMS (Root Mean Square) volume
                    let sum_squares: f32 = frame.iter().map(|&x| x * x).sum();
                    let rms = (sum_squares / FRAME_SIZE as f32).sqrt();

                    // Threshold: 0.01 is a conservative starting point for a noise gate
                    if rms > 0.01 {
                        // 4. Encode to Opus
                        // Max packet size 4000 bytes is plenty for Opus
                        let mut output_param = [0u8; 4000];
                        match encoder.encode_float(&frame, &mut output_param) {
                            Ok(len) => {
                                // Send encoded packet
                                let packet = output_param[..len].to_vec();
                                let _ = audio_tx.send(packet);
                            }
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

                // Simple mixing
                let count = streams.len() as f32;
                // Soft clipping protection: 1.0 / sqrt(N)
                let gain = 1.0 / count.max(1.0).sqrt(); 

                for stream_buf in streams.iter_mut() {
                    for (i, sample) in data.iter_mut().enumerate() {
                        if i < stream_buf.len() {
                            *sample += stream_buf[i] * gain;
                        }
                    }
                }
                
                // Cleanup processed samples
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
        // We will decode immediately here.
        
        let mut decoder = match Decoder::new(SAMPLE_RATE, Channels::Mono) {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut output_buffer = vec![0.0f32; FRAME_SIZE];
        
        match decoder.decode_float(&packet, &mut output_buffer, false) {
            Ok(len) => {
                // Trim to actual decoded length
                output_buffer.truncate(len);
                let mut incoming = self.incoming_audio.write().await;
                incoming.push(output_buffer);
            }
            Err(e) => error!("Opus decode error: {}", e),
        }
    }
}
