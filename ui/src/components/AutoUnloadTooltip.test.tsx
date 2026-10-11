import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import AutoUnloadTooltip from "./AutoUnloadTooltip";
import { useStore } from "../state/store";

const { snoozeAutoUnload, getEngineStatus } = vi.hoisted(() => ({
  snoozeAutoUnload: vi.fn().mockResolvedValue(undefined),
  // The real snoozeAutoUnload follows up with a pollEngine(), so this has to
  // answer with a status the store can actually apply — and a snooze pushes
  // the unload a full idle period out, which is what makes the warning go.
  getEngineStatus: vi.fn(async () => ({
    type: "running" as const,
    data: { modelId: "m1", port: 8080, diskSlow: false, autoUnloadAt: Date.now() + 900_000 },
  })),
}));

vi.mock("../lib/api", () => ({ api: { snoozeAutoUnload, getEngineStatus } }));

/** Puts a loaded engine `secondsOut` seconds away from its idle unload. With
 * no `settings`, the component falls back to a 15-minute idle period, whose
 * warning window is the 60s cap — see `unloadWarningMs`. */
function engineUnloadingIn(secondsOut: number) {
  return {
    type: "running" as const,
    data: {
      modelId: "m1",
      port: 8080,
      diskSlow: false,
      autoUnloadAt: Date.now() + secondsOut * 1000,
    },
  };
}

describe("AutoUnloadTooltip", () => {
  beforeEach(() => {
    snoozeAutoUnload.mockClear();
    getEngineStatus.mockClear();
    useStore.setState({ engine: { type: "stopped" }, settings: null, sidebarMoreMenuOpen: false });
  });

  afterEach(() => {
    useStore.setState({ engine: { type: "stopped" } });
  });

  it("stays out of the way until the unload is close", () => {
    useStore.setState({ engine: engineUnloadingIn(120) });
    render(<AutoUnloadTooltip />);
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();
    // The live region itself stays mounted and empty — assistive tech has to
    // have registered it before it gets text, or the announcement is missed.
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("warns, with a countdown, once inside the warning window", () => {
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    expect(screen.getByText(/unloading your ai in \d+s/i)).toBeInTheDocument();
  });

  it("announces a stable sentence rather than the ticking countdown", () => {
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    // role="status" is implicitly aria-atomic, so a live region carrying the
    // seconds would re-read the whole popover once a second.
    expect(screen.getByRole("status")).toHaveTextContent(/about to unload/i);
    expect(screen.getByRole("status")).not.toHaveTextContent(/\d+s/);
  });

  it("Cancel resets the idle clock instead of turning the feature off", async () => {
    const user = userEvent.setup();
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    // The old "Turn off auto-unload" button is gone — that lives in Settings.
    expect(screen.queryByRole("button", { name: /turn off/i })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /cancel/i }));
    expect(snoozeAutoUnload).toHaveBeenCalledOnce();
    // The follow-up poll picks up the reset clock and the warning stands down.
    await waitFor(() =>
      expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument(),
    );
  });

  it("closing hides it for the rest of this countdown, jitter and all", async () => {
    const user = userEvent.setup();
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);

    await user.click(screen.getByRole("button", { name: /still unload/i }));
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();

    // The backend recomputes autoUnloadAt as `now + (limit - idle_for)` on
    // every 2s poll, so the value drifts a little even when nothing changed.
    // That must not bring the warning back.
    act(() => useStore.setState({ engine: engineUnloadingIn(29) }));
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();
  });

  it("re-arms for the next countdown once the idle clock is actually reset", async () => {
    const user = userEvent.setup();
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    await user.click(screen.getByRole("button", { name: /still unload/i }));

    // Cancel, or just typing, pushes the unload minutes out — leaving the
    // warning window entirely...
    act(() => useStore.setState({ engine: engineUnloadingIn(900) }));
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();

    // ...and the next time it comes back around, the warning shows again.
    act(() => useStore.setState({ engine: engineUnloadingIn(20) }));
    expect(screen.getByText(/unloading your ai in \d+s/i)).toBeInTheDocument();
  });

  it("disappears when the engine stops", () => {
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    act(() => useStore.setState({ engine: { type: "stopped" } }));
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();
  });

  it("yields to the sidebar's More menu, which opens straight across it", () => {
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    act(() => useStore.setState({ sidebarMoreMenuOpen: true }));
    expect(screen.queryByText(/unloading your ai/i)).not.toBeInTheDocument();
    act(() => useStore.setState({ sidebarMoreMenuOpen: false }));
    expect(screen.getByText(/unloading your ai in \d+s/i)).toBeInTheDocument();
  });

  it("names the x by what it does, not just \"close\"", () => {
    useStore.setState({ engine: engineUnloadingIn(30) });
    render(<AutoUnloadTooltip />);
    // Two adjacent affordances with opposite effects — the label has to say
    // which is which.
    expect(screen.getByRole("button", { name: /still unload/i })).toBeInTheDocument();
  });
});
