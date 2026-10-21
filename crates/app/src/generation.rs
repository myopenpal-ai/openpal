//! The app's single foreground-generation queue: every request that puts the
//! model to work (a chat turn on any route, Translate/Correct, the Message
//! improver, the safety classifier) claims it first and holds the claim for
//! as long as it talks to the model. llama-server runs one request at a time
//! here, and OpenPal stays mono-model — so rather than a second slot or a
//! second engine, requests take turns.
//!
//! A claimant that finds the queue busy waits for everything ahead of it,
//! strictly first in, first out, instead of being refused. The frontend keeps
//! its own ordered list of requests (`modelRequests` in `ui/src/state/
//! store.ts`) and only sends one to Rust once it reaches the head of it, so
//! waiting here is normally brief; this queue is what guarantees that no two
//! requests are ever inside the engine at once whatever the frontend does.
//!
//! Each claim carries its own cancel flag. [`GenerationQueue::stop_running`]
//! (the Stop button) sets only the running claim's, so a waiting request is
//! never stopped by a Stop meant for another; [`GenerationQueue::stop_all`]
//! (a reload of the main window) sets every claim's, and a waiting claim
//! whose flag is set leaves the queue without running. Dropping a claim —
//! however the request ended, including a dropped future — releases it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Default)]
pub struct GenerationQueue {
    inner: Mutex<Inner>,
    /// Woken whenever a claim leaves the queue or is cancelled, so waiters
    /// re-check whether it's their turn.
    changed: Notify,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    /// Every claim not yet released, oldest first. The front one is the
    /// request inside the engine; the rest are waiting.
    claims: VecDeque<(u64, Arc<AtomicBool>)>,
}

/// A place in the [`GenerationQueue`], held from the moment it's the
/// caller's turn until it's dropped.
pub struct GenerationClaim<'a> {
    queue: &'a GenerationQueue,
    id: u64,
    cancel: Arc<AtomicBool>,
}

impl GenerationClaim<'_> {
    /// This request's cancel flag — the one [`GenerationQueue::stop_running`]
    /// sets while it runs. Hand it to the engine/stream so a stop reaches it.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

impl Drop for GenerationClaim<'_> {
    fn drop(&mut self) {
        self.queue.remove(self.id);
    }
}

impl GenerationQueue {
    /// Wait for every claim ahead of this one to be released, then return
    /// this one's claim. Returns `None`, without ever running, if the claim
    /// is cancelled while it waits ([`Self::stop_all`]).
    pub async fn claim(&self) -> Option<GenerationClaim<'_>> {
        let cancel = Arc::new(AtomicBool::new(false));
        let id = {
            let mut inner = self.lock();
            let id = inner.next_id;
            inner.next_id += 1;
            inner.claims.push_back((id, cancel.clone()));
            id
        };
        // From here on, dropping `claim` (on cancel, or if this future is
        // dropped mid-wait) takes this place out of the queue again.
        let claim = GenerationClaim {
            queue: self,
            id,
            cancel,
        };
        loop {
            // Registered before checking, so a release between the check and
            // the await still wakes this waiter.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if claim.cancel.load(Ordering::SeqCst) {
                return None;
            }
            if self.lock().claims.front().map(|(front, _)| *front) == Some(id) {
                return Some(claim);
            }
            notified.await;
        }
    }

    /// Claim the queue only if nothing holds it or waits for it — for work
    /// that would rather skip than wait (the request-triage hint).
    pub fn try_claim(&self) -> Option<GenerationClaim<'_>> {
        let mut inner = self.lock();
        if !inner.claims.is_empty() {
            return None;
        }
        let id = inner.next_id;
        inner.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        inner.claims.push_back((id, cancel.clone()));
        Some(GenerationClaim {
            queue: self,
            id,
            cancel,
        })
    }

    /// Stop the request inside the engine, if any — the Stop button. Waiting
    /// claims are untouched and the next one starts once it ends.
    pub fn stop_running(&self) {
        if let Some((_, cancel)) = self.lock().claims.front() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Stop the running request and cancel every waiting one. Used when the
    /// main window (re)loads: the page those requests stream to — and the
    /// card a paused turn waits on — is gone, so nothing could still use
    /// their output, and every wait inside a turn also listens for this flag.
    pub fn stop_all(&self) {
        for (_, cancel) in &self.lock().claims {
            cancel.store(true, Ordering::SeqCst);
        }
        self.changed.notify_waiters();
    }

    fn remove(&self, id: u64) {
        self.lock().claims.retain(|(claim, _)| *claim != id);
        self.changed.notify_waiters();
    }

    /// The state is only ever touched for a few instructions with no await
    /// in between, so a poisoned lock just means another thread panicked
    /// there — keep going with what it left.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Long enough for a spawned task to reach its await, short enough to
    /// keep the suite quick.
    const SETTLE: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_second_claimant_waits_for_the_first_and_then_runs() {
        let queue = Arc::new(GenerationQueue::default());
        let first = queue.claim().await.expect("a free queue claims at once");

        let second = tokio::spawn({
            let queue = queue.clone();
            async move { queue.claim().await.map(|_| ()) }
        });
        tokio::time::sleep(SETTLE).await;
        assert!(
            !second.is_finished(),
            "the second claim waits while the first is held"
        );

        drop(first);
        tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("the second claim runs once the first is released")
            .unwrap()
            .expect("and it isn't refused");
    }

    #[tokio::test]
    async fn claims_run_strictly_first_in_first_out() {
        let queue = Arc::new(GenerationQueue::default());
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = queue.claim().await.unwrap();

        let mut waiters = Vec::new();
        for n in 1..=3 {
            let (queue, order) = (queue.clone(), order.clone());
            waiters.push(tokio::spawn(async move {
                let _claim = queue.claim().await.expect("not cancelled");
                order.lock().unwrap().push(n);
                // Hold it a moment, as a real request would.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
            // Queue them in a known order.
            tokio::time::sleep(SETTLE).await;
        }

        drop(first);
        for waiter in waiters {
            tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn stop_running_stops_only_the_request_in_the_engine() {
        let queue = Arc::new(GenerationQueue::default());
        let running = queue.claim().await.unwrap();
        let waiting = tokio::spawn({
            let queue = queue.clone();
            async move {
                let claim = queue.claim().await.expect("a Stop never cancels a waiter");
                claim.cancel_flag().load(Ordering::SeqCst)
            }
        });
        tokio::time::sleep(SETTLE).await;

        queue.stop_running();
        assert!(running.cancel_flag().load(Ordering::SeqCst));
        drop(running);

        let stopped = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(!stopped, "the next request starts with its own, clear flag");
    }

    #[tokio::test]
    async fn stop_all_cancels_waiters_and_frees_the_queue() {
        let queue = Arc::new(GenerationQueue::default());
        let running = queue.claim().await.unwrap();
        let waiting = tokio::spawn({
            let queue = queue.clone();
            async move { queue.claim().await.is_none() }
        });
        tokio::time::sleep(SETTLE).await;

        // What the reloaded window's page-load hook does.
        queue.stop_all();
        let cancelled = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled, "a waiting claim leaves without running");
        assert!(running.cancel_flag().load(Ordering::SeqCst));
        drop(running);

        queue
            .try_claim()
            .expect("nothing is left holding or waiting on the queue");
    }

    #[tokio::test]
    async fn a_dropped_waiter_gives_up_its_place() {
        let queue = Arc::new(GenerationQueue::default());
        let running = queue.claim().await.unwrap();
        let abandoned = tokio::spawn({
            let queue = queue.clone();
            async move {
                let _ = queue.claim().await;
            }
        });
        tokio::time::sleep(SETTLE).await;
        abandoned.abort();
        let _ = abandoned.await;

        let next = tokio::spawn({
            let queue = queue.clone();
            async move { queue.claim().await.map(|_| ()) }
        });
        drop(running);
        tokio::time::timeout(Duration::from_secs(5), next)
            .await
            .expect("an abandoned waiter never blocks the ones behind it")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn try_claim_skips_rather_than_waits() {
        let queue = GenerationQueue::default();
        let held = queue.claim().await.unwrap();
        assert!(queue.try_claim().is_none());
        drop(held);
        assert!(queue.try_claim().is_some());
    }
}
