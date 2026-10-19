import { beforeEach, describe, expect, it, vi } from "vitest";

const { listen, invoke } = vi.hoisted(() => ({ listen: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, Channel: class {} }));

import { api } from "./api";

beforeEach(() => {
  listen.mockReset();
  invoke.mockReset();
});

// The event name is a string both sides spell by hand (`idle.rs` has a test
// pinning its half); nothing else would notice a typo here, only a nudge that
// never arrives.
describe("api.onEngineStatusChanged", () => {
  it("listens to Rust's engine-status-changed event and calls back without the payload", async () => {
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    const cb = vi.fn();

    const stop = await api.onEngineStatusChanged(cb);

    expect(listen).toHaveBeenCalledTimes(1);
    expect(listen.mock.calls[0][0]).toBe("engine-status-changed");
    listen.mock.calls[0][1]({ event: "engine-status-changed", id: 1, payload: null });
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledWith();
    expect(stop).toBe(unlisten);
  });

  it("resolves to a no-op where there is no event bridge, rather than rejecting", async () => {
    listen.mockRejectedValue(new Error("no transformCallback"));
    const stop = await api.onEngineStatusChanged(() => {});
    expect(() => stop()).not.toThrow();
  });

  it("does the same when listen throws before returning a promise", async () => {
    listen.mockImplementation(() => {
      throw new TypeError("__TAURI_INTERNALS__ is undefined");
    });
    const stop = await api.onEngineStatusChanged(() => {});
    expect(() => stop()).not.toThrow();
  });
});

describe("the notification commands", () => {
  it("names the Rust commands and their arguments as Rust declares them", async () => {
    invoke.mockResolvedValue(undefined);
    await api.notifyNative("Model ready", "Qwen is loaded.");
    expect(invoke).toHaveBeenLastCalledWith("notify_native", {
      title: "Model ready",
      body: "Qwen is loaded.",
      conversationId: null,
    });
    await api.notifyNative("Answer ready", "OpenPal has finished answering.", "c1");
    expect(invoke).toHaveBeenLastCalledWith("notify_native", {
      title: "Answer ready",
      body: "OpenPal has finished answering.",
      conversationId: "c1",
    });

    invoke.mockResolvedValue("denied");
    await expect(api.notificationPermission()).resolves.toBe("denied");
    expect(invoke).toHaveBeenLastCalledWith("notification_permission");

    // The one that may put a system prompt on screen is its own command, so a
    // plain read can never be mistaken for it.
    invoke.mockResolvedValue("granted");
    await expect(api.requestNotificationPermission()).resolves.toBe("granted");
    expect(invoke).toHaveBeenLastCalledWith("request_notification_permission");

    // Sending the user to the OS page that can undo a "denied" is its own
    // command, and it takes nothing: Rust knows which page and which app.
    invoke.mockResolvedValue(undefined);
    await api.openNotificationSettings();
    expect(invoke).toHaveBeenLastCalledWith("open_notification_settings");

    // A click is read, not pushed: the event only says there is one.
    invoke.mockResolvedValue({ conversationId: "c1" });
    await expect(api.takeNotificationClick()).resolves.toEqual({ conversationId: "c1" });
    expect(invoke).toHaveBeenLastCalledWith("take_notification_click");
  });

  it("listens for the click nudge under the name Rust emits, and degrades without a bridge", async () => {
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    const cb = vi.fn();
    const stop = await api.onNotificationClicked(cb);
    expect(listen).toHaveBeenLastCalledWith("notification-clicked", expect.any(Function));
    // The nudge's payload is ignored: the caller reads the click itself.
    (listen.mock.calls.at(-1)?.[1] as (e: unknown) => void)({ payload: "anything" });
    expect(cb).toHaveBeenCalledWith();
    stop();
    expect(unlisten).toHaveBeenCalledTimes(1);

    listen.mockImplementation(() => {
      throw new TypeError("__TAURI_INTERNALS__ is undefined");
    });
    const noop = await api.onNotificationClicked(() => {});
    expect(() => noop()).not.toThrow();
  });
});
