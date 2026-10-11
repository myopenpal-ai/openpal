//! Shared setup for this crate's `#[ignore]`d live-engine integration tests
//! (`engine_e2e.rs`, `writing_tools_e2e.rs`) — downloads the tiny SmolLM2
//! test model if needed and starts the real `llama-server` binary against
//! it. Lives at `tests/support/mod.rs`, not `tests/support.rs`, so cargo
//! doesn't compile it as its own top-level test binary — the standard
//! convention for integration-test helpers shared across files.

use openpal_core::download;
use openpal_core::engine::{EngineManager, STARTUP_TIMEOUT};
use openpal_core::types::EngineConfig;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const MODEL_URL: &str = "https://huggingface.co/lmstudio-community/SmolLM2-135M-Instruct-GGUF/resolve/main/SmolLM2-135M-Instruct-Q8_0.gguf";
const MODEL_BYTES: u64 = 144_811_072;
// From the embedded catalog entry for smollm2-135m.
const MODEL_SHA256: &str = "40f7094960b6ede829145d102ca79451b364b27d9d8694d4406e002024cff357";

/// Download the tiny SmolLM2 test model (a no-op if it's already on disk),
/// start the real `llama-server` binary against it, and return the running
/// [`EngineManager`] plus its log-dir `TempDir` — keep the latter alive for
/// the whole test, since dropping it deletes the directory the engine is
/// still writing logs into. Panics with a clear message on any setup
/// failure, matching this test tier's existing style (these are opt-in,
/// `--ignored` tests; a panic here means the environment isn't set up, not
/// that the feature under test is broken).
pub async fn start_test_engine() -> (EngineManager, tempfile::TempDir) {
    let binary =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/resources/llama/llama-server");
    assert!(
        binary.exists(),
        "engine not staged: run cargo run -p openpal-bundler -- fetch-engine"
    );

    let models_dir = std::env::temp_dir().join("openpal-test-models");
    let model_path = models_dir.join("SmolLM2-135M-Instruct-Q8_0.gguf");
    download::download_file(
        &reqwest::Client::new(),
        MODEL_URL,
        &model_path,
        Some(MODEL_BYTES),
        Some(MODEL_SHA256),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    )
    .await
    .expect("test model download");

    let log_dir = tempfile::tempdir().unwrap();
    let engine = EngineManager::new(log_dir.path().to_path_buf());
    let cfg = EngineConfig {
        context_length: 2048,
        gpu_layers: 99,
        threads: None,
        extra_args: Vec::new(),
        mmproj_path: None,
    };
    engine
        .start(
            &binary,
            "smollm2-135m",
            &model_path,
            &cfg,
            false,
            STARTUP_TIMEOUT,
        )
        .await
        .expect("engine start");

    (engine, log_dir)
}
