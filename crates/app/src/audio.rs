//! Native microphone capture (cpal) and audio playback (rodio) for the Voice
//! plugin.
//!
//! The webview can't do this itself: its locked CSP forbids reaching the
//! plugin's loopback service, and opening the microphone from the webview is
//! unreliable across platforms. So the Voice plugin's audio crosses the IPC
//! boundary as bytes — recorded WAV up to `/transcribe`, synthesized WAV back
//! from `/speak` — and the actual device I/O happens here.
//!
//! Both cpal's `Stream` and rodio's `OutputStream` are `!Send`, so neither may
//! live in `AppState`. Recording runs to completion on a blocking task's own
//! thread (the stream never escapes it). Playback is owned by one long-lived
//! thread we talk to over a channel, so `AppState` holds only the `Sender`
//! (which is `Send + Sync`).

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

const MIC_FAIL: &str = "Couldn't start recording from the microphone. Please try again.";

// ---------------------------------------------------------------------------
// Recording: microphone -> 16-bit PCM WAV bytes
// ---------------------------------------------------------------------------

/// Capture from the default input device until `stop` is set (or `max_secs`
/// elapses), returning the audio as a 16-bit PCM WAV. **Blocking** — call from
/// `spawn_blocking`. cpal's `Stream` never leaves this function, so its
/// `!Send`-ness is fine here.
pub fn record_wav(stop: Arc<AtomicBool>, max_secs: u64) -> Result<Vec<u8>, String> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        "No microphone was found. Check your sound input settings and try again.".to_string()
    })?;
    let supported = device.default_input_config().map_err(|e| {
        tracing::warn!("cpal default input config: {e}");
        "Couldn't open the microphone. Make sure OpenPal is allowed to use it in System \
         Settings → Privacy & Security → Microphone."
            .to_string()
    })?;

    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let cfg: cpal::StreamConfig = supported.clone().into();
    let samples: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let err_fn = |e| tracing::warn!("cpal input stream error: {e}");

    let stream = {
        let samples = samples.clone();
        match supported.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &cfg,
                move |data: &[f32], _: &_| {
                    let mut buf = samples.lock().unwrap();
                    buf.extend(data.iter().map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16));
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &cfg,
                move |data: &[i16], _: &_| samples.lock().unwrap().extend_from_slice(data),
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &cfg,
                move |data: &[u16], _: &_| {
                    let mut buf = samples.lock().unwrap();
                    buf.extend(data.iter().map(|&x| (x as i32 - 32_768) as i16));
                },
                err_fn,
                None,
            ),
            other => return Err(format!("unsupported microphone sample format: {other:?}")),
        }
    }
    .map_err(|e| {
        tracing::warn!("cpal build input stream: {e}");
        MIC_FAIL.to_string()
    })?;

    stream.play().map_err(|e| {
        tracing::warn!("cpal stream play: {e}");
        MIC_FAIL.to_string()
    })?;

    let deadline = Instant::now() + Duration::from_secs(max_secs);
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream); // stop capture

    let buf = samples.lock().unwrap();
    tracing::debug!(
        samples = buf.len(),
        sample_rate,
        channels,
        "audio: microphone capture finished"
    );
    encode_wav(&buf, channels, sample_rate)
}

fn encode_wav(samples: &[i16], channels: u16, sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec).map_err(|e| internal(e, "wav writer"))?;
        for &s in samples {
            writer
                .write_sample(s)
                .map_err(|e| internal(e, "wav sample"))?;
        }
        writer.finalize().map_err(|e| internal(e, "wav finalize"))?;
    }
    Ok(cursor.into_inner())
}

fn internal(e: impl std::fmt::Display, ctx: &str) -> String {
    tracing::error!("audio {ctx}: {e}");
    "Something went wrong while handling audio. Please try again.".to_string()
}

// ---------------------------------------------------------------------------
// Playback: WAV bytes -> speakers
// ---------------------------------------------------------------------------

enum PlayCmd {
    Play(Vec<u8>, Sender<()>),
    Stop,
}

/// Plays WAV audio on one long-lived thread. Each `play` cancels whatever was
/// playing and returns a receiver that fires when the new clip ends — naturally,
/// when replaced by another `play`, or on `stop` — so the caller can await it.
pub struct AudioPlayer {
    tx: Sender<PlayCmd>,
}

impl AudioPlayer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("openpal-audio".into())
            .spawn(move || player_loop(rx))
            .expect("spawn audio playback thread");
        Self { tx }
    }

    /// Play `wav`, replacing anything currently playing. The returned receiver
    /// resolves once this clip is done (finished, replaced, or stopped).
    pub fn play(&self, wav: Vec<u8>) -> Receiver<()> {
        let (done_tx, done_rx) = mpsc::channel();
        if self.tx.send(PlayCmd::Play(wav, done_tx)).is_err() {
            // The audio thread is gone; fire immediately so the caller never hangs.
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(());
            return rx;
        }
        done_rx
    }

    pub fn stop(&self) {
        let _ = self.tx.send(PlayCmd::Stop);
    }
}

impl Default for AudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

fn player_loop(rx: Receiver<PlayCmd>) {
    let (_stream, handle) = match rodio::OutputStream::try_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("no audio output device: {e}");
            // Still drain commands so every caller's receiver fires.
            while let Ok(cmd) = rx.recv() {
                if let PlayCmd::Play(_, done) = cmd {
                    let _ = done.send(());
                }
            }
            return;
        }
    };

    let mut current: Option<(rodio::Sink, Sender<()>)> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(PlayCmd::Play(wav, done)) => {
                if let Some((sink, prev_done)) = current.take() {
                    sink.stop();
                    let _ = prev_done.send(());
                }
                match rodio::Decoder::new(Cursor::new(wav)) {
                    Ok(source) => match rodio::Sink::try_new(&handle) {
                        Ok(sink) => {
                            sink.append(source);
                            tracing::debug!("audio: playback started");
                            current = Some((sink, done));
                        }
                        Err(e) => {
                            tracing::warn!("rodio sink: {e}");
                            let _ = done.send(());
                        }
                    },
                    Err(e) => {
                        tracing::warn!("decode audio: {e}");
                        let _ = done.send(());
                    }
                }
            }
            Ok(PlayCmd::Stop) => {
                if let Some((sink, done)) = current.take() {
                    sink.stop();
                    let _ = done.send(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Has the current clip finished on its own?
                if current.as_ref().is_some_and(|(sink, _)| sink.empty()) {
                    if let Some((_, done)) = current.take() {
                        let _ = done.send(());
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
