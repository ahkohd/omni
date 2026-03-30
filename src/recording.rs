use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub type ChunkSender = mpsc::Sender<Vec<i16>>;

#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub device_name: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub chunk_sender: Option<ChunkSender>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 16_000,
            channels: 1,
            chunk_sender: None,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FinishedRecording {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    samples: Arc<Mutex<Vec<i16>>>,
    sample_rate: u32,
    channels: u16,
    started_at: Instant,
}

impl Recorder {
    pub fn start(config: RecordingConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = select_input_device(&host, config.device_name.as_deref())?;

        let supported = device
            .default_input_config()
            .context("failed getting default input config")?;

        let mut stream_config: cpal::StreamConfig = supported.config();
        stream_config.channels = config.channels;
        stream_config.sample_rate = cpal::SampleRate(config.sample_rate);

        let samples = Arc::new(Mutex::new(Vec::new()));

        let err_fn = |err| {
            eprintln!("omni recorder stream error: {err}");
        };

        let stream = match supported.sample_format() {
            cpal::SampleFormat::I16 => {
                let samples_for_callback = Arc::clone(&samples);
                let sender = config.chunk_sender.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| push_i16(data, &samples_for_callback, sender.as_ref()),
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let samples_for_callback = Arc::clone(&samples);
                let sender = config.chunk_sender.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        push_u16_as_i16(data, &samples_for_callback, sender.as_ref())
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::F32 => {
                let samples_for_callback = Arc::clone(&samples);
                let sender = config.chunk_sender.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        push_f32_as_i16(data, &samples_for_callback, sender.as_ref())
                    },
                    err_fn,
                    None,
                )
            }
            sample_format => {
                return Err(anyhow!(
                    "unsupported input sample format from host: {sample_format:?}"
                ));
            }
        }
        .context("failed building audio input stream")?;

        stream
            .play()
            .context("failed starting audio input stream")?;

        Ok(Self {
            stream: Some(stream),
            samples,
            sample_rate: stream_config.sample_rate.0,
            channels: stream_config.channels,
            started_at: Instant::now(),
        })
    }

    pub fn stop(mut self) -> Result<FinishedRecording> {
        self.stream.take();

        let samples = self
            .samples
            .lock()
            .map_err(|_| anyhow!("audio sample buffer lock poisoned"))?
            .clone();

        let duration_ms = if self.sample_rate == 0 || self.channels == 0 {
            0
        } else {
            let frames = samples.len() as u64 / self.channels as u64;
            (frames * 1000) / self.sample_rate as u64
        };

        let _elapsed = self.started_at.elapsed();

        Ok(FinishedRecording {
            samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
            duration_ms,
        })
    }
}

#[allow(dead_code)]
pub fn wav_bytes(recording: &FinishedRecording) -> Result<Vec<u8>> {
    if recording.channels == 0 {
        bail!("recording channels must be > 0");
    }
    if recording.sample_rate == 0 {
        bail!("recording sample_rate must be > 0");
    }

    let spec = hound::WavSpec {
        channels: recording.channels,
        sample_rate: recording.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec).context("failed creating wav writer")?;

        for sample in &recording.samples {
            writer
                .write_sample(*sample)
                .context("failed writing wav sample")?;
        }

        writer.finalize().context("failed finalizing wav writer")?;
    }

    Ok(cursor.into_inner())
}

fn select_input_device(host: &cpal::Host, requested_name: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = requested_name
        && name != "default"
    {
        let devices = host
            .input_devices()
            .context("failed listing input audio devices")?;
        for device in devices {
            if let Ok(device_name) = device.name()
                && device_name == name
            {
                return Ok(device);
            }
        }

        bail!(
            "requested input device not found: {name}. Run `omni input list` then `omni input set <id>` (or `omni input set default`)"
        );
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("no default audio input device available"))
}

fn push_i16(data: &[i16], buffer: &Arc<Mutex<Vec<i16>>>, sender: Option<&ChunkSender>) {
    if let Ok(mut guard) = buffer.lock() {
        guard.extend_from_slice(data);
    }

    if let Some(sender) = sender {
        let _ = sender.send(data.to_vec());
    }
}

fn push_u16_as_i16(data: &[u16], buffer: &Arc<Mutex<Vec<i16>>>, sender: Option<&ChunkSender>) {
    let converted: Vec<i16> = data.iter().map(|v| (*v as i32 - 32768) as i16).collect();

    if let Ok(mut guard) = buffer.lock() {
        guard.extend_from_slice(&converted);
    }

    if let Some(sender) = sender {
        let _ = sender.send(converted);
    }
}

fn push_f32_as_i16(data: &[f32], buffer: &Arc<Mutex<Vec<i16>>>, sender: Option<&ChunkSender>) {
    let converted: Vec<i16> = data
        .iter()
        .map(|v| {
            let clamped = v.clamp(-1.0, 1.0);
            (clamped * i16::MAX as f32) as i16
        })
        .collect();

    if let Ok(mut guard) = buffer.lock() {
        guard.extend_from_slice(&converted);
    }

    if let Some(sender) = sender {
        let _ = sender.send(converted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_bytes_contains_data_for_samples() {
        let recording = FinishedRecording {
            samples: vec![0, 1000, -1000, 0],
            sample_rate: 16_000,
            channels: 1,
            duration_ms: 0,
        };

        let wav = wav_bytes(&recording).expect("wav conversion should work");
        assert!(wav.len() > 44, "wav should contain header + sample data");
    }
}
