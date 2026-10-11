//! Mobile stand-in for `audio.rs` — same public API, no cpal/rodio.
//!
//! Voice (STT/TTS) is dropped from mobile v1 (see `docs/design/MOBILE.md`'s
//! "What's cut, not ported" table): every caller already returns a friendly
//! "Turn on Voice in Add-ons" error before reaching these, since mobile
//! resolves no plugins at all. This file exists only so `crates/app` compiles
//! for Android/iOS — cpal's Android backend links Google's Oboe C++ library
//! (`oboe-sys`), which needs the NDK's libc++ wired up for no functionality
//! OpenPal ships on mobile yet; simplest to not link it at all.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

pub fn record_wav(_stop: Arc<AtomicBool>, _max_secs: u64) -> Result<Vec<u8>, String> {
    Err("Voice isn't available on mobile yet.".to_string())
}

pub struct AudioPlayer;

impl AudioPlayer {
    pub fn new() -> Self {
        Self
    }

    /// Never actually called (see module docs), but fires immediately rather
    /// than hanging if it ever is.
    pub fn play(&self, _wav: Vec<u8>) -> Receiver<()> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(());
        rx
    }

    pub fn stop(&self) {}
}

impl Default for AudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}
