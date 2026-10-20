//! OpenPal core: everything the app does that isn't GUI.
//!
//! This crate is deliberately free of Tauri (and any GUI) dependencies so the
//! same logic can back the desktop app, a CLI, or tests. The GUI crate
//! (`crates/app`) is a thin command layer over this crate, and the
//! bundling/deployment module (`bundler/`) is a separate crate entirely.
//!
//! Modules:
//! - [`agent`] — agent mode: sandboxed/consent-gated tool use (shell, code,
//!   filesystem) for the local model.
//! - [`account`] — sign-in and cached premium/cloud-model entitlements.
//! - [`hardware`] — detect the user's machine (RAM, chip, cores, Metal, disk).
//! - [`catalog`] — the curated, embedded catalog of known-good GGUF models.
//! - [`custom`] — user-added models (any Hugging Face GGUF or a local file).
//! - [`huggingface`] — resolve pasted Hugging Face links and list a repo's GGUFs.
//! - [`gguf_meta`] — parse a GGUF file's own header for real MoE/param-count data.
//! - [`selection`] — pick the best model for this machine, with plain-English reasons.
//! - [`disk_speed`] — probe the model volume's speed for `FitLevel::DiskSlow`.
//! - [`download`] — resumable, cancellable model downloads with progress.
//! - [`favicon`] — best-effort site icons for web-search sources (as data URIs).
//! - [`engine`] — manage a `llama-server` subprocess and stream chat completions.
//! - [`cloud`] — chat with a signed-in account's cloud model instead of the
//!   local engine.
//! - [`sse`] — the OpenAI-compatible chat SSE parser shared by `engine` and
//!   `cloud`.
//! - [`plugin`] — runtime plugin framework (enable/disable, setup, services, chat).
//! - [`settings`] — persisted user settings.
//! - [`store`] — persisted conversations.
//! - [`projects`] — persisted projects (named groups of conversations).
//! - [`prompt_enhancer`] — fixed system prompt + sampling + output-cleaning
//!   for the composer's "Improve my message" action, run against the
//!   already-loaded model (never a second model) — same shape as
//!   `writing_tools`, but the output replaces the user's own draft rather
//!   than filling a separate result pane.
//! - [`remote`] — Remote Access: pair a device, discover it over mDNS, and
//!   proxy chat completions to it over the LAN (or an already-connected
//!   overlay network), plus wake-on-LAN.
//! - [`attachments`] — turn a picked file into a chat attachment (image data
//!   URI, or extracted document text).
//! - [`update`] — check a published manifest for a newer OpenPal version.
//! - [`update_portable`] — download, verify, and apply an update in place
//!   for the Portable SKU (Full uses Tauri's own updater plugin instead).
//! - [`engine_update`] — download, verify, and stage a newer llama.cpp
//!   engine build on its own signed channel, independent of app releases.
//! - [`injection`] — prompt-injection detection: a dedicated, always-on
//!   classifier (a knowing exception to mono-model first) that catches text
//!   trying to give the model orders. Flagged attachments, skills and tool
//!   results wait on a review card (Continue or Stop) before the model reads
//!   them; typed text gets a notice in the reply; both flag the next consent
//!   card and write one app-log line.
//! - [`writing_tools`] — fixed system prompts + sampling for the Writing
//!   Tools plugin's Translate/Correct commands, run against the already-
//!   loaded model (never a second, translation-specialized one).

pub mod account;
pub mod agent;
pub mod app_db;
pub mod applock;
pub mod artifacts;
pub mod attachments;
pub mod boot;
pub mod catalog;
pub mod cloud;
pub mod crypto;
pub mod custom;
pub mod disk_speed;
pub mod download;
pub mod engine;
pub mod engine_update;
pub mod error;
pub mod export;
pub mod favicon;
pub mod gguf_meta;
pub mod gpu;
pub mod hardware;
pub mod huggingface;
pub mod import;
pub mod injection;
pub mod parental;
pub mod plugin;
pub mod projects;
pub mod prompt_enhancer;
pub mod remote;
pub mod selection;
pub mod settings;
pub mod skills;
pub mod sse;
pub mod store;
pub mod sync;
pub mod types;
pub mod update;
pub mod update_portable;
pub mod writing_tools;

pub use error::{Error, Result};
