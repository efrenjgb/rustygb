//! Plays the emulator's audio through the default output device using cpal.
//!
//! The emulator pushes samples into a shared queue and the audio device pulls
//! them out from its own thread. The queue length also tells the main loop
//! when to run the next frame, so the sound card's clock sets the pace.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct AudioOutput {
    queue: Arc<Mutex<VecDeque<f32>>>,
    pub sample_rate: u32,
    /// Set when the stream stops working, for example when the default
    /// output device changes because headphones were plugged in.
    failed: Arc<AtomicBool>,
    /// The stream stops playing when dropped, so keep it alive.
    _stream: cpal::Stream,
}

impl AudioOutput {
    /// Opens the default output device. Returns an error message if there is
    /// no usable device, in which case the emulator runs silently.
    pub fn new() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let supported = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let sample_rate = supported.sample_rate();
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let failed = Arc::new(AtomicBool::new(false));

        let config = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, config, &queue, &failed),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, config, &queue, &failed),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, config, &queue, &failed),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        stream.play().map_err(|error| error.to_string())?;

        Ok(AudioOutput {
            queue,
            sample_rate,
            failed,
            _stream: stream,
        })
    }

    /// Adds interleaved stereo samples to the end of the queue.
    pub fn push(&self, samples: &[f32]) {
        self.queue.lock().unwrap().extend(samples);
    }

    /// Drops anything still waiting to be played.
    pub fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }

    /// Whether the stream has stopped working and needs to be opened again.
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// How many stereo frames are waiting to be played.
    pub fn queued_frames(&self) -> usize {
        self.queue.lock().unwrap().len() / 2
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    queue: &Arc<Mutex<VecDeque<f32>>>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let queue = Arc::clone(queue);
    let failed = Arc::clone(failed);

    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                let mut queue = queue.lock().unwrap();
                for frame in output.chunks_mut(channels) {
                    // If the emulator falls behind, play silence instead of stalling.
                    let left = queue.pop_front().unwrap_or(0.0);
                    let right = queue.pop_front().unwrap_or(0.0);
                    for (i, sample) in frame.iter_mut().enumerate() {
                        let value = match (channels, i) {
                            (1, _) => (left + right) / 2.0,
                            (_, 0) => left,
                            (_, 1) => right,
                            _ => 0.0,
                        };
                        *sample = T::from_sample(value);
                    }
                }
            },
            move |error| {
                eprintln!("audio error: {error}");
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|error| error.to_string())
}
