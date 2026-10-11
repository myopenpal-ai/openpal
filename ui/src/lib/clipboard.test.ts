import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { copyTextCmd } = vi.hoisted(() => ({ copyTextCmd: vi.fn() }));
vi.mock("./api", () => ({ api: { copyText: copyTextCmd } }));

import { copyText } from "./clipboard";

describe("copyText", () => {
  const writeText = vi.fn();

  beforeEach(() => {
    copyTextCmd.mockReset();
    writeText.mockReset();
    Object.assign(navigator, { clipboard: { writeText } });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns true when the Rust clipboard command succeeds", async () => {
    copyTextCmd.mockResolvedValue(undefined);
    await expect(copyText("hello")).resolves.toBe(true);
    expect(copyTextCmd).toHaveBeenCalledWith("hello");
    expect(writeText).not.toHaveBeenCalled();
  });

  it("falls back to navigator.clipboard when the Rust command rejects", async () => {
    copyTextCmd.mockRejectedValue(new Error("no Tauri IPC"));
    writeText.mockResolvedValue(undefined);
    await expect(copyText("hello")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");
  });

  it("returns false when both the Rust command and the clipboard API reject", async () => {
    copyTextCmd.mockRejectedValue(new Error("no Tauri IPC"));
    writeText.mockRejectedValue(new Error("clipboard denied"));
    await expect(copyText("hello")).resolves.toBe(false);
  });
});
