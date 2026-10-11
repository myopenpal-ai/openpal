//! Resumable, cancellable file downloads with progress reporting.
//!
//! ## Behavior contract
//!
//! - Downloads stream to `<dest>.part`, then atomically rename to `dest` on
//!   success. `dest` existing and complete → emit `Started` + `Finished`
//!   immediately and return Ok.
//! - If `<dest>.part` exists, resume with a `Range: bytes=<len>-` header.
//!   - Server replies `206` → append from there; `Started.resumed_from_bytes`
//!     is the existing length.
//!   - Server replies `200` (range ignored) → truncate and restart from 0.
//!   - `416` → the .part is likely complete or bogus: delete it and restart.
//! - Emit `Progress` at most ~5x/second (don't flood the IPC channel), with a
//!   smoothed `bytes_per_sec` (exponential moving average) and a stable
//!   `eta_seconds`.
//! - Check `cancel` (AtomicBool, SeqCst) between chunks — and on a short
//!   timer while waiting for the next chunk, so a stalled connection can
//!   still be cancelled. On cancel: flush, KEEP the .part file (so we can
//!   resume later), emit `Cancelled`, return `Err(Error::Cancelled)`.
//! - On success, if `expected_bytes` is Some and the final size differs,
//!   delete the file and fail with a friendly message (corrupt download).
//! - If `expected_sha256` is Some, every written byte is also streamed
//!   through SHA-256 (resumes first hash the existing .part contents); a
//!   digest mismatch after completion deletes the file and fails the same
//!   way. The already-complete-`dest` short circuit does not re-hash.
//! - Network errors mid-stream: keep the .part, emit `Failed` with a friendly
//!   message like "The download was interrupted. Check your internet
//!   connection and try again. It will pick up where it left off."
//! - Always also return the failure as `Err(...)`; events and the return
//!   value must agree.
//!
//! HuggingFace `resolve` URLs redirect (302) to a CDN — reqwest follows
//! redirects by default; make sure the Range header survives the redirect
//! (set it on the request; reqwest re-applies headers on same-method
//! redirects).

use crate::types::DownloadEvent;
use crate::{Error, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time::Instant;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How long to wait for the next chunk before re-checking the cancel flag,
/// so a stalled connection can still be cancelled promptly.
const STALL_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Weight of the newest speed sample in the moving average.
const EMA_ALPHA: f64 = 0.3;

const INTERRUPTED_MSG: &str = "The download was interrupted. Check your internet connection and \
                               try again. It will pick up where it left off.";
const CONNECT_MSG: &str =
    "The download couldn't start. Check your internet connection and try again.";
const DISK_MSG: &str =
    "The file couldn't be saved. Check that you have enough free disk space and try again.";
const CORRUPT_MSG: &str = "The download didn't come through correctly, so the incomplete file \
                           was removed. Please try again.";

/// Download `url` to `dest` per the module behavior contract.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_bytes: Option<u64>,
    expected_sha256: Option<&str>,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(DownloadEvent) + Send,
) -> Result<()> {
    // Already fully downloaded? Nothing to do.
    if let Ok(meta) = std::fs::metadata(dest) {
        if expected_bytes.is_none_or(|expected| expected == meta.len()) {
            on_event(DownloadEvent::Started {
                total_bytes: meta.len(),
                resumed_from_bytes: meta.len(),
            });
            on_event(DownloadEvent::Finished);
            return Ok(());
        }
        // Wrong size: not the file we want. Start over.
        let _ = std::fs::remove_file(dest);
    }

    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!("could not create download directory {parent:?}: {e}");
            return Err(fail(&mut on_event, DISK_MSG));
        }
    }

    let part = part_path(dest);
    let mut resume_from = partial_bytes(dest);

    let response = loop {
        let mut request = client.get(url);
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("download request failed: {e}");
                return Err(fail(&mut on_event, CONNECT_MSG));
            }
        };
        match response.status() {
            reqwest::StatusCode::RANGE_NOT_SATISFIABLE if resume_from > 0 => {
                // The .part is complete or bogus: delete it and restart.
                let _ = tokio::fs::remove_file(&part).await;
                resume_from = 0;
            }
            reqwest::StatusCode::PARTIAL_CONTENT => break response,
            status if status.is_success() => {
                // Range ignored (or none sent): the body is the whole file.
                resume_from = 0;
                break response;
            }
            status => {
                tracing::warn!("download rejected: {status} for {url}");
                let message = format!(
                    "The download couldn't start (the server replied with code {}). \
                     Please try again in a few minutes.",
                    status.as_u16()
                );
                return Err(fail(&mut on_event, &message));
            }
        }
    };

    let total_bytes = match response.content_length() {
        Some(remaining) => resume_from + remaining,
        None => expected_bytes.unwrap_or(0),
    };
    on_event(DownloadEvent::Started {
        total_bytes,
        resumed_from_bytes: resume_from,
    });

    // Stream every written byte through SHA-256 when a checksum is expected;
    // a resume must first hash what's already in the .part file.
    let mut hasher = expected_sha256.map(|_| Sha256::new());
    if resume_from > 0 {
        if let Some(h) = hasher.take() {
            match prehash_part(part.clone(), h).await {
                Ok(h) => hasher = Some(h),
                Err(e) => {
                    tracing::warn!("could not hash existing {part:?}: {e}");
                    return Err(fail(&mut on_event, DISK_MSG));
                }
            }
        }
    }

    let file = if resume_from > 0 {
        tokio::fs::OpenOptions::new().append(true).open(&part).await
    } else {
        tokio::fs::File::create(&part).await
    };
    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("could not open {part:?}: {e}");
            return Err(fail(&mut on_event, DISK_MSG));
        }
    };

    let mut received = resume_from;
    let mut last_emit = Instant::now();
    let mut last_received = received;
    let mut bytes_per_sec = 0.0_f64;
    let mut stream = response.bytes_stream();

    loop {
        // Wait for the next chunk only briefly at a time: if the connection
        // stalls without erroring, we still notice a cancel and stop.
        let next = match tokio::time::timeout(STALL_POLL_INTERVAL, stream.next()).await {
            Ok(Some(next)) => next,
            Ok(None) => break, // stream finished
            Err(_elapsed) => {
                if cancel.load(Ordering::SeqCst) {
                    let _ = file.flush().await;
                    on_event(DownloadEvent::Cancelled);
                    return Err(Error::Cancelled);
                }
                continue;
            }
        };
        if cancel.load(Ordering::SeqCst) {
            let _ = file.flush().await;
            on_event(DownloadEvent::Cancelled);
            return Err(Error::Cancelled);
        }
        let chunk = match next {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = file.flush().await;
                tracing::warn!("download stream error after {received} bytes: {e}");
                return Err(fail(&mut on_event, INTERRUPTED_MSG));
            }
        };
        if let Err(e) = file.write_all(&chunk).await {
            tracing::warn!("download write error: {e}");
            return Err(fail(&mut on_event, DISK_MSG));
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&chunk);
        }
        received += chunk.len() as u64;

        let elapsed = last_emit.elapsed();
        if elapsed >= PROGRESS_INTERVAL {
            let sample = (received - last_received) as f64 / elapsed.as_secs_f64();
            bytes_per_sec = if bytes_per_sec == 0.0 {
                sample
            } else {
                EMA_ALPHA * sample + (1.0 - EMA_ALPHA) * bytes_per_sec
            };
            let eta_seconds = if bytes_per_sec > 0.0 && total_bytes > received {
                ((total_bytes - received) as f64 / bytes_per_sec).round() as u64
            } else {
                0
            };
            on_event(DownloadEvent::Progress {
                received_bytes: received,
                total_bytes,
                bytes_per_sec,
                eta_seconds,
            });
            last_emit = Instant::now();
            last_received = received;
        }
    }

    if let Err(e) = file.flush().await {
        tracing::warn!("download flush error: {e}");
        return Err(fail(&mut on_event, DISK_MSG));
    }
    drop(file);

    let final_len = std::fs::metadata(&part)
        .map(|m| m.len())
        .unwrap_or(received);
    if let Some(expected) = expected_bytes {
        if final_len != expected {
            tracing::warn!("download size mismatch: got {final_len}, expected {expected}");
            let _ = std::fs::remove_file(&part);
            return Err(fail(&mut on_event, CORRUPT_MSG));
        }
    }

    if let (Some(expected), Some(hasher)) = (expected_sha256, hasher) {
        let digest = format!("{:x}", hasher.finalize());
        if !digest.eq_ignore_ascii_case(expected) {
            tracing::warn!("download sha256 mismatch: got {digest}, expected {expected}");
            let _ = std::fs::remove_file(&part);
            return Err(fail(&mut on_event, CORRUPT_MSG));
        }
    }

    if let Err(e) = tokio::fs::rename(&part, dest).await {
        tracing::warn!("could not move {part:?} into place: {e}");
        return Err(fail(&mut on_event, DISK_MSG));
    }
    on_event(DownloadEvent::Finished);
    Ok(())
}

/// Bytes already present in `<dest>.part` (0 if none) — used by the UI to show
/// "Resume" state without starting a download.
pub fn partial_bytes(dest: &Path) -> u64 {
    std::fs::metadata(part_path(dest))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Feed the existing `.part` contents into `hasher` before a resume appends
/// to it, so the final digest covers the whole file. Runs off the async
/// runtime — a large partial can take a moment to read.
async fn prehash_part(part: PathBuf, mut hasher: Sha256) -> std::io::Result<Sha256> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&part)?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                return Ok(hasher);
            }
            hasher.update(&buf[..n]);
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

/// `X.gguf` -> `X.gguf.part` (".part" is appended to the full file name).
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Emit a `Failed` event and build the matching error, so events and the
/// return value always agree.
fn fail(on_event: &mut impl FnMut(DownloadEvent), message: &str) -> Error {
    on_event(DownloadEvent::Failed {
        message: message.to_string(),
    });
    Error::Download(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of `b"abcdef"` (the body served by [`serve_once`] tests).
    const ABCDEF_SHA256: &str = "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721";

    /// One-shot loopback HTTP server: accepts a single connection, reads the
    /// request headers, replies with `status_line` + `body`, and returns the
    /// URL to request. Keeps the sha256 tests free of any real network.
    fn serve_once(status_line: &'static str, body: &'static [u8]) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = stream.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
                if n == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = format!(
                "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });
        format!("http://{addr}/model.gguf")
    }

    #[test]
    fn part_path_appends_to_the_full_file_name() {
        let dest = Path::new("/models/X.gguf");
        assert_eq!(part_path(dest), Path::new("/models/X.gguf.part"));
        // crates/app cleans up partials via with_extension("gguf.part").
        assert_eq!(part_path(dest), dest.with_extension("gguf.part"));
    }

    #[test]
    fn partial_bytes_reads_the_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        assert_eq!(partial_bytes(&dest), 0);
        std::fs::write(part_path(&dest), b"12345").unwrap();
        assert_eq!(partial_bytes(&dest), 5);
    }

    #[tokio::test]
    async fn complete_dest_short_circuits_without_any_network() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        std::fs::write(&dest, b"abcdef").unwrap();

        let mut events = Vec::new();
        // The URL is unreachable on purpose: it must never be contacted.
        download_file(
            &reqwest::Client::new(),
            "http://127.0.0.1:9/never",
            &dest,
            Some(6),
            None,
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await
        .unwrap();

        assert!(matches!(
            events[0],
            DownloadEvent::Started {
                total_bytes: 6,
                resumed_from_bytes: 6
            }
        ));
        assert!(matches!(events[1], DownloadEvent::Finished));
    }

    #[tokio::test]
    async fn matching_sha256_passes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let url = serve_once("HTTP/1.1 200 OK", b"abcdef");

        let mut events = Vec::new();
        download_file(
            &reqwest::Client::new(),
            &url,
            &dest,
            Some(6),
            Some(ABCDEF_SHA256),
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await
        .unwrap();

        assert!(matches!(events.last(), Some(DownloadEvent::Finished)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
    }

    #[tokio::test]
    async fn sha256_mismatch_deletes_the_file_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let url = serve_once("HTTP/1.1 200 OK", b"abcdeX");

        let mut events = Vec::new();
        let result = download_file(
            &reqwest::Client::new(),
            &url,
            &dest,
            Some(6),
            Some(ABCDEF_SHA256),
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await;

        assert!(matches!(result, Err(Error::Download(_))));
        assert!(matches!(events.last(), Some(DownloadEvent::Failed { .. })));
        assert!(!dest.exists(), "corrupt download must be removed");
        assert_eq!(partial_bytes(&dest), 0, "the .part must be removed too");
    }

    #[tokio::test]
    async fn connection_failure_emits_failed_and_matching_error() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");

        let mut events = Vec::new();
        let result = download_file(
            &reqwest::Client::new(),
            "http://127.0.0.1:9/never",
            &dest,
            Some(10),
            None,
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await;

        assert!(matches!(result, Err(Error::Download(_))));
        assert!(matches!(events.last(), Some(DownloadEvent::Failed { .. })));
    }
}
