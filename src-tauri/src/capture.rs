use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ltc::{Decoder, Frame, FrameRate, RateDetector};

const POLL_INTERVAL: Duration = Duration::from_millis(300);
const LEVEL_SMOOTHING: f32 = 0.1;
const CHANNEL_BUFFER_CAPACITY: usize = 16_384;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(clippy::derivable_impls)]
impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::Unknown
    }
}

#[derive(Clone, Debug, Default)]
pub struct CaptureObservation {
    pub started_at_ms: Option<u64>,
    pub last_sample_at_ms: Option<u64>,
    pub last_frame_at_ms: Option<u64>,
    pub last_frame: Option<Frame>,
    pub level: f32,
    pub detected_rate: FrameRate,
    pub stream_error: Option<String>,
}

#[derive(Default)]
struct Infrequent {
    last_frame: Option<Frame>,
    detected_rate: FrameRate,
    stream_error: Option<String>,
}

pub struct CaptureState {
    started_at_ms: AtomicU64,
    last_sample_at_ms: AtomicU64,
    last_frame_at_ms: AtomicU64,
    level_bits: AtomicU32,
    infrequent: Mutex<Infrequent>,
}

impl CaptureState {
    fn new() -> Self {
        Self {
            started_at_ms: AtomicU64::new(0),
            last_sample_at_ms: AtomicU64::new(0),
            last_frame_at_ms: AtomicU64::new(0),
            level_bits: AtomicU32::new(0f32.to_bits()),
            infrequent: Mutex::new(Infrequent::default()),
        }
    }

    fn reset(&self) {
        self.started_at_ms.store(0, Ordering::Relaxed);
        self.last_sample_at_ms.store(0, Ordering::Relaxed);
        self.last_frame_at_ms.store(0, Ordering::Relaxed);
        self.level_bits.store(0f32.to_bits(), Ordering::Relaxed);
        if let Ok(mut guard) = self.infrequent.lock() {
            guard.last_frame = None;
            guard.detected_rate = FrameRate::Unknown;
            guard.stream_error = None;
        }
    }

    fn mark_started(&self) {
        self.started_at_ms.store(now_ms(), Ordering::Relaxed);
        self.last_sample_at_ms.store(0, Ordering::Relaxed);
        self.last_frame_at_ms.store(0, Ordering::Relaxed);
        self.level_bits.store(0f32.to_bits(), Ordering::Relaxed);
        if let Ok(mut guard) = self.infrequent.lock() {
            guard.last_frame = None;
            guard.detected_rate = FrameRate::Unknown;
            guard.stream_error = None;
        }
    }

    fn set_error(&self, message: String) {
        if let Ok(mut guard) = self.infrequent.lock() {
            guard.stream_error = Some(message);
        }
    }

    // Called only from the realtime audio callback: non-blocking, non-allocating.
    fn publish_sample(&self, level: f32) {
        self.last_sample_at_ms.store(now_ms(), Ordering::Relaxed);
        self.level_bits.store(level.to_bits(), Ordering::Relaxed);
    }

    // Called only from the realtime audio callback: non-blocking, non-allocating.
    fn publish_frame(&self, frame: Frame, rate: FrameRate) {
        self.last_frame_at_ms.store(now_ms(), Ordering::Relaxed);
        if let Ok(mut guard) = self.infrequent.try_lock() {
            guard.last_frame = Some(frame);
            guard.detected_rate = rate;
        }
    }

    // Called only from the realtime error callback: non-blocking, non-allocating.
    fn publish_error(&self, message: &'static str) {
        if let Ok(mut guard) = self.infrequent.try_lock() {
            guard.stream_error = Some(message.to_string());
        }
    }

    pub fn observation(&self) -> CaptureObservation {
        let started = self.started_at_ms.load(Ordering::Relaxed);
        let last_sample = self.last_sample_at_ms.load(Ordering::Relaxed);
        let last_frame_at = self.last_frame_at_ms.load(Ordering::Relaxed);
        let level = f32::from_bits(self.level_bits.load(Ordering::Relaxed));
        let (last_frame, detected_rate, stream_error) = match self.infrequent.lock() {
            Ok(guard) => (
                guard.last_frame,
                guard.detected_rate,
                guard.stream_error.clone(),
            ),
            Err(_) => (
                None,
                FrameRate::Unknown,
                Some("capture state lock is poisoned".into()),
            ),
        };
        CaptureObservation {
            started_at_ms: (started != 0).then_some(started),
            last_sample_at_ms: (last_sample != 0).then_some(last_sample),
            last_frame_at_ms: (last_frame_at != 0).then_some(last_frame_at),
            last_frame,
            level,
            detected_rate,
            stream_error,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureTarget {
    pub device_id: String,
    pub channel: u16,
}

enum CaptureCommand {
    Configure(CaptureTarget),
    Disable,
    Shutdown,
}

pub struct CaptureManager {
    command_tx: mpsc::Sender<CaptureCommand>,
    shared: Arc<CaptureState>,
    join: Option<thread::JoinHandle<()>>,
}

impl CaptureManager {
    pub fn start() -> Self {
        let shared = Arc::new(CaptureState::new());
        let (command_tx, command_rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let join = thread::spawn(move || run(command_rx, thread_shared));
        Self {
            command_tx,
            shared,
            join: Some(join),
        }
    }

    pub fn configure(&self, target: Option<CaptureTarget>) {
        let command = match target {
            Some(target) => CaptureCommand::Configure(target),
            None => CaptureCommand::Disable,
        };
        let _ = self.command_tx.send(command);
    }

    pub fn observation(&self) -> CaptureObservation {
        self.shared.observation()
    }
}

impl Drop for CaptureManager {
    fn drop(&mut self) {
        let _ = self.command_tx.send(CaptureCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(command_rx: mpsc::Receiver<CaptureCommand>, shared: Arc<CaptureState>) {
    let mut current: Option<CaptureTarget> = None;
    let mut stream: Option<cpal::Stream> = None;

    loop {
        match command_rx.recv_timeout(POLL_INTERVAL) {
            Ok(CaptureCommand::Configure(target)) => {
                if current.as_ref() != Some(&target) {
                    stream = None;
                    current = Some(target.clone());
                    shared.mark_started();
                    match build_stream(&target, Arc::clone(&shared)) {
                        Ok(built) => stream = Some(built),
                        Err(error) => shared.set_error(error.message()),
                    }
                }
            }
            Ok(CaptureCommand::Disable) => {
                stream = None;
                current = None;
                shared.reset();
            }
            Ok(CaptureCommand::Shutdown) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(target) = &current
                    && stream.is_none()
                {
                    shared.mark_started();
                    match build_stream(target, Arc::clone(&shared)) {
                        Ok(built) => stream = Some(built),
                        Err(error) => shared.set_error(error.message()),
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

enum BuildError {
    DeviceNotFound,
    ChannelExceedsDeviceChannels,
    UnsupportedSampleFormat(cpal::SampleFormat),
    ConfigUnavailable(String),
    BuildFailed(String),
    PlayFailed(String),
}

impl BuildError {
    fn message(&self) -> String {
        match self {
            BuildError::DeviceNotFound => {
                "The configured audio input device could not be opened.".to_string()
            }
            BuildError::ChannelExceedsDeviceChannels => {
                "The configured channel exceeds the input device's channel count.".to_string()
            }
            BuildError::UnsupportedSampleFormat(format) => {
                format!("The audio input reported an unsupported sample format: {format}.")
            }
            BuildError::ConfigUnavailable(detail) => {
                format!("Could not read the audio input configuration: {detail}")
            }
            BuildError::BuildFailed(detail) => {
                format!("Could not open the audio input stream: {detail}")
            }
            BuildError::PlayFailed(detail) => {
                format!("Could not start the audio input stream: {detail}")
            }
        }
    }
}

fn find_device(device_id: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let devices = host.input_devices().ok()?;
    devices.into_iter().find(|device| {
        device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_default()
            == device_id
    })
}

struct CallbackState {
    decoder: Decoder,
    rate_detector: RateDetector,
    channel_buffer: Vec<f32>,
    frame_out: Vec<Frame>,
    channel_index: usize,
    channels: usize,
    sample_rate: u32,
    smoothed_level: f32,
    shared: Arc<CaptureState>,
}

impl CallbackState {
    fn new(
        channel_index: usize,
        channels: usize,
        sample_rate: u32,
        shared: Arc<CaptureState>,
    ) -> Self {
        Self {
            decoder: Decoder::new(),
            rate_detector: RateDetector::new(),
            channel_buffer: Vec::with_capacity(CHANNEL_BUFFER_CAPACITY),
            frame_out: Vec::with_capacity(8),
            channel_index,
            channels,
            sample_rate,
            smoothed_level: 0.0,
            shared,
        }
    }

    fn handle<T: Copy>(&mut self, data: &[T], convert: impl Fn(T) -> f32) {
        self.channel_buffer.clear();
        if self.channels != 0 {
            let mut index = self.channel_index;
            let capacity = self.channel_buffer.capacity();
            while index < data.len() {
                if self.channel_buffer.len() >= capacity {
                    break;
                }
                self.channel_buffer.push(convert(data[index]));
                index += self.channels;
            }
        }

        self.frame_out.clear();
        self.decoder
            .push_into(&self.channel_buffer, self.sample_rate, &mut self.frame_out);

        let mut peak = 0f32;
        for &sample in &self.channel_buffer {
            let magnitude = sample.abs();
            if magnitude > peak {
                peak = magnitude;
            }
        }
        self.smoothed_level += (peak - self.smoothed_level) * LEVEL_SMOOTHING;
        self.shared.publish_sample(self.smoothed_level);

        if let Some(&frame) = self.frame_out.last() {
            self.rate_detector
                .push(frame.sample_index, frame.drop_frame);
            let rate = self.rate_detector.detect(self.sample_rate);
            self.shared.publish_frame(frame, rate);
        }
    }
}

fn build_stream(
    target: &CaptureTarget,
    shared: Arc<CaptureState>,
) -> Result<cpal::Stream, BuildError> {
    let device = find_device(&target.device_id).ok_or(BuildError::DeviceNotFound)?;
    let supported = device
        .default_input_config()
        .map_err(|error| BuildError::ConfigUnavailable(error.to_string()))?;
    let channels = supported.channels();
    if target.channel == 0 || target.channel > channels {
        return Err(BuildError::ChannelExceedsDeviceChannels);
    }
    let sample_format = supported.sample_format();
    let config = supported.config();
    let sample_rate = config.sample_rate;
    let channel_index = (target.channel - 1) as usize;
    let channels_usize = channels as usize;

    let error_shared = Arc::clone(&shared);
    let error_callback = move |_error: cpal::Error| {
        error_shared.publish_error(
            "The audio input stream reported an error, possibly because the device was disconnected.",
        );
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut state = CallbackState::new(channel_index, channels_usize, sample_rate, shared);
            device
                .build_input_stream_raw(
                    config,
                    sample_format,
                    move |data, _info| {
                        if let Some(samples) = data.as_slice::<f32>() {
                            state.handle(samples, |sample| sample);
                        }
                    },
                    error_callback,
                    None,
                )
                .map_err(|error| BuildError::BuildFailed(error.to_string()))?
        }
        cpal::SampleFormat::I16 => {
            let mut state = CallbackState::new(channel_index, channels_usize, sample_rate, shared);
            device
                .build_input_stream_raw(
                    config,
                    sample_format,
                    move |data, _info| {
                        if let Some(samples) = data.as_slice::<i16>() {
                            state.handle(samples, |sample| sample as f32 / i16::MAX as f32);
                        }
                    },
                    error_callback,
                    None,
                )
                .map_err(|error| BuildError::BuildFailed(error.to_string()))?
        }
        cpal::SampleFormat::U16 => {
            let mut state = CallbackState::new(channel_index, channels_usize, sample_rate, shared);
            device
                .build_input_stream_raw(
                    config,
                    sample_format,
                    move |data, _info| {
                        if let Some(samples) = data.as_slice::<u16>() {
                            state.handle(samples, |sample| (sample as f32 - 32_768.0) / 32_768.0);
                        }
                    },
                    error_callback,
                    None,
                )
                .map_err(|error| BuildError::BuildFailed(error.to_string()))?
        }
        other => return Err(BuildError::UnsupportedSampleFormat(other)),
    };

    stream
        .play()
        .map_err(|error| BuildError::PlayFailed(error.to_string()))?;

    Ok(stream)
}
