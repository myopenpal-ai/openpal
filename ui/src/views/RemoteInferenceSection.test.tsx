import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "../i18n";
import type { RemoteInferenceStatus } from "../lib/types";
import { RemoteInferenceSection } from "./SettingsSheet";

const { remoteInferenceStatus, remoteInferenceConnect, remoteInferenceDisconnect } = vi.hoisted(
  () => ({
    remoteInferenceStatus: vi.fn(),
    remoteInferenceConnect: vi.fn(),
    remoteInferenceDisconnect: vi.fn().mockResolvedValue(undefined),
  }),
);

// The section is the only thing reaching over the seam here; anything else
// still throws, so a stray call surfaces as a failing test.
vi.mock("../lib/api", () => ({
  api: { remoteInferenceStatus, remoteInferenceConnect, remoteInferenceDisconnect },
}));

const pairing = {
  deviceId: "d1",
  deviceName: "Big Mac",
  hostAddr: "192.168.1.20",
  port: 7443,
  fingerprintHex: "aa:bb:cc",
  pairedAtMs: 42,
};

function status(overrides: Partial<RemoteInferenceStatus> = {}): RemoteInferenceStatus {
  return { active: false, pairing: null, engine: { type: "stopped" }, ...overrides };
}

function renderSection() {
  return render(<RemoteInferenceSection />);
}

describe("RemoteInferenceSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(async () => {
    await i18n.changeLanguage("en");
  });

  it("starts with the manual address/port/code form when there's no pairing", async () => {
    remoteInferenceStatus.mockResolvedValue(status());
    renderSection();

    const address = await screen.findByRole("textbox", { name: "Address" });
    expect(address).toBeInTheDocument();
    expect(address).toHaveAttribute("placeholder", "e.g. 192.168.1.20");
    expect(screen.getByRole("textbox", { name: "Port" })).toHaveAttribute("placeholder", "e.g. 7443");
    expect(screen.getByRole("textbox", { name: "Pairing code" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Disconnect" })).not.toBeInTheDocument();
  });

  it("connects with the typed address, port, and code, then shows the routed device", async () => {
    remoteInferenceStatus.mockResolvedValue(status());
    remoteInferenceConnect.mockResolvedValue(
      status({ active: true, pairing, engine: { type: "running", data: { modelId: "qwen2.5-7b", port: 8080, diskSlow: false, autoUnloadAt: null } } }),
    );
    renderSection();

    await userEvent.type(await screen.findByRole("textbox", { name: "Address" }), "192.168.1.20");
    await userEvent.type(screen.getByRole("textbox", { name: "Port" }), "7443");
    await userEvent.type(screen.getByRole("textbox", { name: "Pairing code" }), "1234");
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    expect(remoteInferenceConnect).toHaveBeenCalledWith("192.168.1.20", 7443, "1234");
    expect(await screen.findByRole("button", { name: "Disconnect" })).toBeInTheDocument();
    // The device line and the which-model note share one hint node, so match
    // each as its own fragment rather than one exact string.
    expect(screen.getByText(/Routed to Big Mac \(192\.168\.1\.20:7443\)\./)).toBeInTheDocument();
    expect(screen.getByText(/Chat runs on qwen2\.5-7b\./)).toBeInTheDocument();
  });

  it("keeps the button disabled and labeled Connecting… while a pairing is in flight", async () => {
    remoteInferenceStatus.mockResolvedValue(status());
    let resolveConnect!: (s: RemoteInferenceStatus) => void;
    remoteInferenceConnect.mockImplementation(
      () => new Promise<RemoteInferenceStatus>((resolve) => (resolveConnect = resolve)),
    );
    renderSection();

    await userEvent.type(await screen.findByRole("textbox", { name: "Address" }), "192.168.1.20");
    await userEvent.type(screen.getByRole("textbox", { name: "Port" }), "7443");
    await userEvent.type(screen.getByRole("textbox", { name: "Pairing code" }), "1234");
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    const pending = await screen.findByRole("button", { name: "Connecting…" });
    expect(pending).toBeDisabled();

    resolveConnect(
      status({
        active: true,
        pairing,
        engine: { type: "starting", data: { modelId: "qwen2.5-72b", progressPercent: null } },
      }),
    );
    expect(await screen.findByRole("button", { name: "Disconnect" })).toBeInTheDocument();
    expect(screen.getByText(/Loading qwen2.5-72b on the other device…/)).toBeInTheDocument();
  });

  it("reports the remote engine as unreachable while staying connected", async () => {
    remoteInferenceStatus.mockResolvedValue(
      status({
        active: true,
        pairing,
        engine: { type: "remoteUnreachable", data: { hostName: "Big Mac" } },
      }),
    );
    renderSection();

    expect(await screen.findByRole("button", { name: "Disconnect" })).toBeInTheDocument();
    expect(screen.getByText(/Can't reach Big Mac/)).toBeInTheDocument();
  });

  it("lets the user disconnect from a pairing the other device has already revoked", async () => {
    remoteInferenceStatus.mockResolvedValue(
      status({ active: true, pairing, engine: { type: "remoteUnpaired" } }),
    );
    renderSection();

    expect(await screen.findByRole("button", { name: "Disconnect" })).toBeInTheDocument();
    expect(screen.getByText(/ended this connection/)).toBeInTheDocument();
  });

  it("disconnects and lands back on the manual form, with the pairing forgotten", async () => {
    remoteInferenceStatus.mockResolvedValue(
      status({ active: true, pairing, engine: { type: "running", data: { modelId: "qwen2.5-7b", port: 8080, diskSlow: false, autoUnloadAt: null } } }),
    );
    renderSection();

    await userEvent.click(await screen.findByRole("button", { name: "Disconnect" }));
    expect(remoteInferenceDisconnect).toHaveBeenCalledTimes(1);
    // Disconnect also forgets the pairing (Rust side), so the form is back.
    expect(await screen.findByRole("textbox", { name: "Address" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
  });

  it("reuses a saved pairing when not routed, without asking for fields again", async () => {
    // A pairing survived a restart, but the engine surface is local again.
    remoteInferenceStatus.mockResolvedValue(status({ pairing }));
    remoteInferenceConnect.mockResolvedValue(
      status({ active: true, pairing, engine: { type: "stopped" } }),
    );
    renderSection();

    expect(await screen.findByText("Routed to Big Mac (192.168.1.20:7443).")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Address" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(remoteInferenceConnect).toHaveBeenCalledWith("", 0, "");
    expect(await screen.findByRole("button", { name: "Disconnect" })).toBeInTheDocument();
  });

  it("shows why a connect failed on the form", async () => {
    remoteInferenceStatus.mockResolvedValue(status());
    remoteInferenceConnect.mockRejectedValue("That code looks wrong.");
    renderSection();

    await userEvent.type(await screen.findByRole("textbox", { name: "Address" }), "192.168.1.20");
    await userEvent.type(screen.getByRole("textbox", { name: "Port" }), "7443");
    await userEvent.type(screen.getByRole("textbox", { name: "Pairing code" }), "0000");
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("That code looks wrong.");
    // The form is still there for a retry.
    expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
  });

  it("refreshes the which-model line while connected, like the engine pill", async () => {
    vi.useFakeTimers();
    try {
      remoteInferenceStatus
        .mockResolvedValueOnce(
          status({
            active: true,
            pairing,
            engine: { type: "starting", data: { modelId: "qwen2.5-72b", progressPercent: null } },
          }),
        )
        .mockResolvedValue(
          status({
            active: true,
            pairing,
            engine: {
              type: "running",
              data: { modelId: "qwen2.5-7b", port: 8080, diskSlow: false, autoUnloadAt: null },
            },
          }),
        );
      renderSection();

      // Flush the initial status promise (no findBy under fake timers — its
      // internal polling is faked too).
      await act(async () => {});
      expect(screen.getByRole("button", { name: "Disconnect" })).toBeInTheDocument();
      expect(screen.getByText(/Loading qwen2\.5-72b/)).toBeInTheDocument();

      // The 2s poll re-reads the status; the stalled engine (started by
      // somebody else) is now running.
      await act(async () => {
        vi.advanceTimersByTime(2000);
      });
      await act(async () => {});
      expect(screen.queryByText(/Loading qwen2\.5-72b/)).toBeNull();
      expect(screen.getByText(/Chat runs on qwen2\.5-7b\./)).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("translates the section into French", async () => {
    remoteInferenceStatus.mockResolvedValue(status());
    await i18n.changeLanguage("fr");
    renderSection();

    expect(
      await screen.findByRole("button", { name: "Se connecter" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Adresse" })).toHaveAttribute(
      "placeholder",
      "ex. 192.168.1.20",
    );
    expect(screen.getByRole("heading", { name: "Inférence à distance" })).toBeInTheDocument();
  });
});
