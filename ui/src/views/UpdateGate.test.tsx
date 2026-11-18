import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import "../i18n";
import UpdateGate from "./UpdateGate";
import { useStore } from "../state/store";

// The gate's only actions route through the store's install flow, which hits
// `api` over IPC — not exercised here (the ticket's frontend test pins the
// gate's *presence*, not the download).
vi.mock("../lib/api", () => ({ api: {} }));

function resetGate() {
  useStore.setState({
    updateGate: { appBlockedVersion: null, engineBlockedTag: null },
    updateGateSettled: false,
    updateStatus: "idle",
    updateProgress: null,
    updateError: null,
  });
}

describe("UpdateGate", () => {
  beforeEach(() => {
    resetGate();
  });

  it("is absent for a safe (un-gated) version", () => {
    const { container } = render(<UpdateGate />);
    expect(container).toBeEmptyDOMElement();
  });

  it("replaces the UI for a gated version, with the update path as the ONLY way out", () => {
    useStore.setState({
      updateGate: { appBlockedVersion: "0.3.14", engineBlockedTag: null },
    });

    render(<UpdateGate />);

    expect(
      screen.getByRole("heading", { name: "This version of OpenPal is no longer supported" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("This version is missing a critical fix. Update now to keep using OpenPal."),
    ).toBeInTheDocument();

    // The one and only control: install the safe version.
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Update now" })).toBeInTheDocument();

    // No skip, no "Later", no dismiss — a gated version has no way past.
    expect(screen.queryByRole("button", { name: "Skip this version" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Later" })).not.toBeInTheDocument();
  });

  it("stays up (retry, not dismiss) on an install failure", () => {
    useStore.setState({
      updateGate: { appBlockedVersion: "0.3.14", engineBlockedTag: null },
      updateStatus: "error",
      updateError: "Couldn't download or install the update. Please try again.",
    });

    render(<UpdateGate />);

    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toBeInTheDocument();
    // Still no way out of the gate.
    expect(screen.queryByRole("button", { name: "Skip this version" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Later" })).not.toBeInTheDocument();
  });
});
