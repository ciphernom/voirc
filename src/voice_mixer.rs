use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use crossbeam::queue::ArrayQueue;
use opus::{Application, Channels, Decoder, Encoder};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use webrtc_audio_processing::{
    Config, EchoCancellation, EchoCancellationSuppressionLevel,
    InitializationConfig, NoiseSuppression, NoiseSuppressionLevel, Processor,
};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS_COUNT: u16 = 1;
const FRAME_SIZE: usize = 960;  // 20ms @ 48kHz — Opus frame
const AEC_FRAME: usize = webrtc_audio_processing::NUM_SAMPLES_PER_FRAME as usize;
const RING_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// AEC internals
// ---------------------------------------------------------------------------

struct AecState {
    processor: Processor,
}

/// Output-callback side: feeds the AEC its echo reference signal.
struct RenderHalf {
    state: Arc<Mutex<AecState>>,
    buffer: VecDeque<f32>,
}

impl RenderHalf {
    fn queue(&mut self, data: &[f32]) {
        self.buffer.extend(data);
        while self.buffer.len() >= AEC_FRAME {
            let mut chunk: Vec<f32> = self.buffer.drain(..AEC_FRAME).collect();
            // try_lock: render runs on the OS audio thread where blocking is
            // unacceptable. Skipping one analysis frame has negligible AEC
            // quality impact; a speaker stutter does not.
            if let Ok(mut s) = self.state.try_lock() {
                if let Err(e) = s.processor.process_render_frame(&mut chunk) {
                    error!("AEC render error: {}", e);
                }
            }
        }
    }
}

/// Input-callback side: removes echo from mic audio.
struct CaptureHalf {
    state: Arc<Mutex<AecState>>,
    buffer: VecDeque<f32>,
}

impl CaptureHalf {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buffer.extend(input);
        while self.buffer.len() >= AEC_FRAME {
            let mut chunk: Vec<f32> = self.buffer.drain(..AEC_FRAME).collect();
            // Blocking lock acceptable here: input feeds a jitter buffer,
            // so a brief wait does not cause audible glitches.
            match self.state.lock() {
                Ok(mut s) => match s.processor.process_capture_frame(&mut chunk) {
                    Ok(_) => out.extend_from_slice(&chunk),
                    Err(e) => {
                        error!("AEC capture error: {}", e);
                        out.extend_from_slice(&chunk); // pass through on error
                    }
                },
                Err(_) => out.extend_from_slice(&chunk), // poisoned — pass through
            }
        }
    }
}

fn build_aec() -> Result<(RenderHalf, CaptureHalf)> {
    // From compiler: InitializationConfig fields are only
    // enable_experimental_agc and enable_intelligibility_enhancer.
    // Sample rate and channel count are not configurable here in 0.5.x;
    // the processor always operates at 48kHz mono when built bundled.
    let init = InitializationConfig {
        ..Default::default()
    };
    let mut processor = Processor::new(&init)?;

    // From compiler: Config fields are echo_cancellation, gain_control,
    // noise_suppression, voice_detection, enable_transient_suppressor,
    // enable_high_pass_filter. EchoCancellation and NoiseSuppression do
    // not implement Default — construct them field-by-field.
    let cfg = Config {
        echo_cancellation: Some(EchoCancellation {
            suppression_level: EchoCancellationSuppressionLevel::High,
            enable_delay_agnostic: true,
            enable_extended_filter: true,
            stream_delay_ms: None,
        }),
        noise_suppression: Some(NoiseSuppression {
            suppression_level: NoiseSuppressionLevel::High,
        }),
        enable_high_pass_filter: true,
        ..Default::default()
    };
    processor.set_config(cfg);

    let shared = Arc::new(Mutex::new(AecState { processor }));

    let render = RenderHalf {
        state: Arc::clone(&shared),
        buffer: VecDeque::with_capacity(AEC_FRAME * 4),
    };
    let capture = CaptureHalf {
        state: Arc::clone(&shared),
        buffer: VecDeque::with_capacity(AEC_FRAME * 4),
    };

    Ok((render, capture))
}

// ---------------------------------------------------------------------------
// Public API — identical signatures to original voice_mixer.rs
// ---------------------------------------------------------------------------

pub struct VoiceMixer {
    input_device: Option<Device>,
    output_device: Device,
    config: StreamConfig,
    net_ring: Arc<ArrayQueue<Vec<f32>>>,
    render_half: Mutex<Option<RenderHalf>>,
    capture_half: Mutex<Option<CaptureHalf>>,
}

pub struct PeerDecoders {
    decoders: HashMap<String, Decoder>,
}

impl PeerDecoders {
    pub fn new() -> Self {
        Self { decoders: HashMap::new() }
    }

    pub fn decode(&mut self, nick: &str, packet: &[u8]) -> Option<Vec<f32>> {
        let decoder = self
            .decoders
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

        let input_device = match host.default_input_device() {
            Some(dev) => {
                info!("Input device: {:?}", dev.name());
                Some(dev)
            }
            None => {
                warn!("No input device found - running in listen-only mode");
                None
            }
        };

        let output_device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No output device"))?;

        let config = StreamConfig {
            channels: CHANNELS_COUNT,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // AEC is best-effort — if it fails (e.g. unsupported sample rate),
        // fall back gracefully. Voice still works, just without echo cancellation.
        let (render, capture) = match build_aec() {
            Ok((r, c)) => {
                info!("AEC initialized");
                (Some(r), Some(c))
            }
            Err(e) => {
                warn!("AEC init failed ({}), running without echo cancellation", e);
                (None, None)
            }
        };

        Ok(Self {
            input_device,
            output_device,
            config,
            net_ring: Arc::new(ArrayQueue::new(RING_CAPACITY)),
            render_half: Mutex::new(render),
            capture_half: Mutex::new(capture),
        })
    }

    pub fn soft_clip(sample: f32) -> f32 {
        (sample * 1.5).tanh()
    }

    pub fn has_input(&self) -> bool {
        self.input_device.is_some()
    }

    pub fn start_input(&self, audio_tx: mpsc::UnboundedSender<Vec<u8>>) -> Result<Option<Stream>> {
        let input_device = match &self.input_device {
            Some(dev) => dev,
            None => {
                info!("No mic - skipping input stream");
                return Ok(None);
            }
        };

        let mut capture_half_opt = self.capture_half.lock().unwrap().take();


        let config = self.config.clone();
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        encoder.set_inband_fec(true)?;
        encoder.set_dtx(true)?;
        let mut opus_acc: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 2);

        let stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if let Some(ref mut ch) = capture_half_opt {
                    ch.process(data, &mut opus_acc);
                } else {
                    opus_acc.extend_from_slice(data);
                }

                while opus_acc.len() >= FRAME_SIZE {
                    let frame: Vec<f32> = opus_acc.drain(..FRAME_SIZE).collect();
                    let rms = (frame.iter().map(|&x| x * x).sum::<f32>() / FRAME_SIZE as f32)
                        .sqrt();
                    if rms > 0.01 {
                        let mut out = [0u8; 4000];
                        match encoder.encode_float(&frame, &mut out) {
                            Ok(len) => {
                                let _ = audio_tx.send(out[..len].to_vec());
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
        Ok(Some(stream))
    }

    pub fn start_output(&self) -> Result<Stream> {
        let config = self.config.clone();
        let net_ring = Arc::clone(&self.net_ring);

        let mut render_half_opt = self.render_half.lock().unwrap().take();


        let stream = self.output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);

                while let Some(buf) = net_ring.pop() {
                    for (i, s) in data.iter_mut().enumerate() {
                        if i < buf.len() {
                            *s += buf[i];
                        }
                    }
                }

                for s in data.iter_mut() {
                    *s = Self::soft_clip(*s);
                }

                if let Some(ref mut rh) = render_half_opt {
                    rh.queue(data);
                }
            },
            |err| error!("Speaker error: {}", err),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn queue_audio(&self, pcm: Vec<f32>) {
        let _ = self.net_ring.push(pcm);
    }
}
