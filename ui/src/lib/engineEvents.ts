/**
 * Turns the stream of engine statuses the UI sees into "a model finished
 * loading" and "a model was unloaded" events — each exactly once per
 * occurrence, however many times the same status is read.
 *
 * Why this is a transition tracker and not "alert when status is X": the
 * status is read over and over (the 2s poll, the refetch after every
 * start/stop/delete, a nudge from the Rust idle loop), so `running` and
 * `stopped` are observed many times per real event. Only a *change* is news,
 * and a swap from model A to model B is one change ("B loaded"), not two
 * ("A unloaded", "B loaded") — even though the raw statuses in between include
 * a transient `stopped` (`SubprocessBackend::start()` stops the old process
 * before it starts the new one) and, from a poll that raced the swap, a stale
 * `running(A)`.
 *
 * Pure: no store, no api, no i18n — so it is tested with plain status values.
 * The store feeds it from a module-level subscriber (see `store.ts`), because
 * that sees every writer of `engine` without each one having to remember to.
 */

import type { EngineStatus } from "./types";

export type EngineAlertEvent =
  | { kind: "modelLoaded"; modelId: string; occurrence: number }
  | { kind: "modelUnloaded"; modelId: string; occurrence: number };

export interface EngineTracker {
  /** The model last seen Running with no load in flight — what an
   * unexpected `stopped` would unload. Null when unknown or when a load is
   * replacing it. */
  running: string | null;
  /** The model last seen Starting that has not been seen Running yet. While
   * set, a `stopped` is a step of a swap (or an aborted load), not an unload. */
  loading: string | null;
  /** Counts the events handed out, so each carries a distinct number the
   * announcer can build a dedupe key from. */
  occurrence: number;
}

export interface EngineStepContext {
  /** `engineBusy`: a `useModel`/`stopModel` call is in flight right now. A
   * `stopped` seen while a load is in flight and the call has ended can only
   * be an aborted load, so the tracker stops waiting for it. */
  busy: boolean;
  /** No events, but the tracker still follows the status. Android (the
   * status there mirrors the paired desktop's engine, which is not this
   * device's model), an update that has finished downloading and is about to
   * restart the app (the portable build stops the engine on the way out), and
   * the moments before settings have loaded. */
  suppress: boolean;
}

/** A fresh tracker: it knows nothing, so the first status it is fed is a
 * silent baseline. Exported as a factory so each test gets its own. */
export function createEngineTracker(): EngineTracker {
  return { running: null, loading: null, occurrence: 0 };
}

/** Feed the next observed status; returns the events it completes (zero or
 * one today) and updates `t` in place. */
export function stepEngineTracker(
  t: EngineTracker,
  next: EngineStatus,
  ctx: EngineStepContext,
): EngineAlertEvent[] {
  const out: EngineAlertEvent[] = [];
  switch (next.type) {
    case "starting":
      // A load (or a reload of the same model) began. Whatever was running is
      // being replaced, not unloaded — its `stopped` is part of this swap.
      t.loading = next.data.modelId;
      t.running = null;
      break;
    case "running": {
      const id = next.data.modelId;
      if (t.loading === id) {
        out.push({ kind: "modelLoaded", modelId: id, occurrence: ++t.occurrence });
        t.loading = null;
      }
      // A first read, a webview reload on a running engine, or a stale
      // `running(A)` from a poll that raced a swap to B: arm the model so a
      // later unload is noticed, but say nothing — nothing was seen loading.
      t.running = id;
      break;
    }
    case "stopped":
      if (t.loading !== null) {
        // Mid-swap flicker, or a load that was aborted. Only the second ends
        // the wait, and only once the start call itself is over.
        if (!ctx.busy) t.loading = null;
        t.running = null;
      } else if (t.running !== null) {
        out.push({ kind: "modelUnloaded", modelId: t.running, occurrence: ++t.occurrence });
        t.running = null;
      }
      break;
    default:
      // failed | remoteUnpaired | remoteUnreachable. Failures are not alerted
      // (out of scope), and nothing after one is a continuation of what came
      // before it: start over.
      t.loading = null;
      t.running = null;
  }
  return ctx.suppress ? [] : out;
}
