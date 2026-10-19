import { describe, expect, it } from "vitest";
import {
  createEngineTracker,
  type EngineAlertEvent,
  type EngineStepContext,
  stepEngineTracker,
} from "./engineEvents";
import type { EngineStatus } from "./types";

const stopped: EngineStatus = { type: "stopped" };
const starting = (modelId: string): EngineStatus => ({
  type: "starting",
  data: { modelId, progressPercent: null },
});
const running = (modelId: string, autoUnloadAt: number | null = null): EngineStatus => ({
  type: "running",
  data: { modelId, port: 8080, diskSlow: false, autoUnloadAt },
});
const failed: EngineStatus = { type: "failed", data: { message: "boom" } };

const idle: EngineStepContext = { busy: false, suppress: false };
const busy: EngineStepContext = { busy: true, suppress: false };
const suppressed: EngineStepContext = { busy: false, suppress: true };

/** Feed a sequence of statuses (each with its own context, default idle) and
 * collect every event, so a test reads as the scenario it describes. */
function run(steps: (EngineStatus | [EngineStatus, EngineStepContext])[]) {
  const tracker = createEngineTracker();
  const events: EngineAlertEvent[] = [];
  for (const step of steps) {
    const [status, ctx] = Array.isArray(step) ? step : [step, idle];
    events.push(...stepEngineTracker(tracker, status, ctx));
  }
  return { tracker, events };
}

const kinds = (events: EngineAlertEvent[]) => events.map((e) => `${e.kind}:${e.modelId}`);

describe("stepEngineTracker", () => {
  describe("a model finishing loading", () => {
    it("reports loaded once, however many times Running is read afterwards", () => {
      const { events } = run([
        stopped,
        starting("a"),
        running("a"),
        running("a"),
        running("a", 123),
        running("a", 456),
        running("a"),
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:a"]);
    });

    it("counts a Starting that is only ever seen locally (the optimistic one)", () => {
      const { events } = run([stopped, [starting("a"), busy], [running("a"), busy]]);
      expect(kinds(events)).toEqual(["modelLoaded:a"]);
    });

    it("reports a load that began before this tracker saw anything (webview reload mid-load)", () => {
      const { events } = run([starting("a"), starting("a"), running("a")]);
      expect(kinds(events)).toEqual(["modelLoaded:a"]);
    });

    it("reports a restart of the same model as a load", () => {
      const { events } = run([
        stopped,
        starting("a"),
        running("a"),
        // "Restart to apply context length".
        starting("a"),
        running("a"),
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:a", "modelLoaded:a"]);
    });
  });

  describe("a model being unloaded", () => {
    it("reports unloaded once, however many times Stopped is read afterwards", () => {
      const { events } = run([
        stopped,
        starting("a"),
        running("a"),
        stopped,
        stopped,
        stopped,
        stopped,
        stopped,
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:a", "modelUnloaded:a"]);
    });

    it("reports an unload of a model that was already running when the tracker started", () => {
      const { events } = run([running("a"), running("a"), stopped, stopped]);
      expect(kinds(events)).toEqual(["modelUnloaded:a"]);
    });

    it("reports a second unload only after a second load", () => {
      const { events } = run([
        running("a"),
        stopped,
        stopped,
        starting("a"),
        running("a"),
        stopped,
        stopped,
      ]);
      expect(kinds(events)).toEqual(["modelUnloaded:a", "modelLoaded:a", "modelUnloaded:a"]);
    });
  });

  describe("the first status seen is a silent baseline", () => {
    it("says nothing for an already-running engine", () => {
      const { events } = run([running("a"), running("a"), running("a")]);
      expect(events).toEqual([]);
    });

    it("says nothing for a stopped engine", () => {
      const { events } = run([stopped, stopped, stopped]);
      expect(events).toEqual([]);
    });

    it("says nothing when Running turns up with no Starting seen before it", () => {
      const { events } = run([stopped, running("a")]);
      expect(events).toEqual([]);
    });
  });

  describe("swapping one model for another", () => {
    it("reports only the new model: no unload for the old one", () => {
      const { events } = run([
        running("a"),
        [starting("b"), busy],
        [running("b"), busy],
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:b"]);
    });

    it("ignores the transient Stopped a start produces (it stops the old process first)", () => {
      const { events } = run([
        running("a"),
        [starting("b"), busy],
        [stopped, busy],
        [starting("b"), busy],
        [running("b"), busy],
        [running("b"), idle],
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:b"]);
    });

    it("ignores a stale Running(old) from a poll that raced the swap", () => {
      const { events } = run([
        running("a"),
        [starting("b"), busy],
        [running("a"), busy],
        [starting("b"), busy],
        [running("b"), busy],
        [running("b"), idle],
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:b"]);
    });

    it("reports the new model's unload afterwards, not the old one's", () => {
      const { events } = run([
        running("a"),
        [starting("b"), busy],
        [running("b"), busy],
        stopped,
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:b", "modelUnloaded:b"]);
    });
  });

  describe("failures and other states", () => {
    it("says nothing for a load that fails, then reports the retry once", () => {
      const { events } = run([
        stopped,
        [starting("a"), busy],
        [failed, busy],
        failed,
        [starting("a"), busy],
        [running("a"), busy],
        running("a"),
      ]);
      expect(kinds(events)).toEqual(["modelLoaded:a"]);
    });

    it("does not take a Running that follows a failed load for the end of that load", () => {
      const { events } = run([[starting("a"), busy], [failed, busy], [running("a"), idle]]);
      expect(events).toEqual([]);
    });

    it("says nothing when a running engine crashes (Running -> Failed -> Stopped)", () => {
      const { events } = run([running("a"), failed, stopped, stopped]);
      expect(events).toEqual([]);
    });

    it("says nothing for Failed -> Stopped", () => {
      const { events } = run([failed, stopped, stopped]);
      expect(events).toEqual([]);
    });

    it("forgets a model when the status turns remote (Android) or unpaired", () => {
      const { events } = run([
        running("a"),
        { type: "remoteUnreachable", data: { hostName: "Desktop" } },
        stopped,
        running("a"),
        { type: "remoteUnpaired" },
        stopped,
      ]);
      expect(events).toEqual([]);
    });
  });

  describe("an aborted load", () => {
    it("stops waiting for a load that ended in Stopped once the start call is over", () => {
      const { events } = run([
        [starting("a"), busy],
        [stopped, idle],
        // Something else now reports Running(a): no Starting was seen for it.
        running("a"),
      ]);
      expect(events).toEqual([]);
    });

    it("keeps waiting while the start call is still in flight", () => {
      const { events } = run([[starting("a"), busy], [stopped, busy], [running("a"), busy]]);
      expect(kinds(events)).toEqual(["modelLoaded:a"]);
    });

    it("does not report the old model as unloaded when its replacement never came up", () => {
      const tracker = createEngineTracker();
      stepEngineTracker(tracker, running("a"), idle);
      stepEngineTracker(tracker, starting("b"), busy);
      // The start call ended without another status being read.
      tracker.loading = null;
      expect(stepEngineTracker(tracker, stopped, idle)).toEqual([]);
    });

    it("forgets the wait when the caller clears `loading` as the call ends", () => {
      const tracker = createEngineTracker();
      stepEngineTracker(tracker, starting("a"), busy);
      stepEngineTracker(tracker, stopped, busy);
      // What the store's subscriber does when `engineBusy` falls true -> false.
      tracker.loading = null;
      expect(stepEngineTracker(tracker, running("a"), idle)).toEqual([]);
    });
  });

  describe("suppress", () => {
    it("returns no events but keeps following the status", () => {
      const tracker = createEngineTracker();
      expect(stepEngineTracker(tracker, starting("a"), suppressed)).toEqual([]);
      expect(stepEngineTracker(tracker, running("a"), suppressed)).toEqual([]);
      // The load was consumed while suppressed, so it is not reported late...
      expect(stepEngineTracker(tracker, running("a"), idle)).toEqual([]);
      // ...and the tracker knows the model is up, so its unload is reported.
      expect(kinds(stepEngineTracker(tracker, stopped, idle))).toEqual(["modelUnloaded:a"]);
    });

    it("drops the unload that happens while suppressed, and does not repeat it", () => {
      const tracker = createEngineTracker();
      stepEngineTracker(tracker, running("a"), idle);
      expect(stepEngineTracker(tracker, stopped, suppressed)).toEqual([]);
      expect(stepEngineTracker(tracker, stopped, idle)).toEqual([]);
    });
  });

  it("gives every event its own occurrence number", () => {
    const { events } = run([
      stopped,
      starting("a"),
      running("a"),
      stopped,
      starting("b"),
      running("b"),
      stopped,
    ]);
    const occurrences = events.map((e) => e.occurrence);
    expect(occurrences).toEqual([1, 2, 3, 4]);
  });

  it("gives each tracker its own state", () => {
    const first = createEngineTracker();
    const second = createEngineTracker();
    stepEngineTracker(first, running("a"), idle);
    expect(stepEngineTracker(second, stopped, idle)).toEqual([]);
    expect(kinds(stepEngineTracker(first, stopped, idle))).toEqual(["modelUnloaded:a"]);
  });
});
