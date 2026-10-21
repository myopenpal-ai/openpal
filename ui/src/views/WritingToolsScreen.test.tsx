import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import WritingToolsScreen from "./WritingToolsScreen";
import { resetAlertTracking, useStore } from "../state/store";
import type { ChatEvent } from "../lib/types";

const { correctStream, stopGeneration } = vi.hoisted(() => ({
  correctStream: vi.fn(),
  stopGeneration: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/api", () => ({ api: { correctStream, stopGeneration } }));

/** Stand-in for a chat reply already using the model: a request at the head
 * of the store's queue that ends when `finish` is called. */
function replyRunning() {
  let finish!: () => void;
  const done = useStore
    .getState()
    .runModelRequest(
      { id: "reply", kind: "improve" },
      () => new Promise<void>((resolve) => (finish = resolve)),
    );
  return async () => {
    finish();
    await act(async () => {
      await done;
    });
  };
}

beforeEach(() => {
  resetAlertTracking();
  useStore.setState({ modelRequests: [], streaming: false });
  stopGeneration.mockClear();
  correctStream.mockReset();
  correctStream.mockImplementation(async (_text: string, onEvent: (e: ChatEvent) => void) => {
    onEvent({ type: "token", data: { text: "Fixed text." } });
  });
});

describe("WritingToolsScreen while a chat reply is running", () => {
  it("waits for it, shown as waiting, then runs", async () => {
    const finishReply = replyRunning();
    const user = userEvent.setup();
    render(<WritingToolsScreen mode="correct" />);

    await user.type(screen.getByLabelText("Your text"), "fixd text");
    const run = screen.getByRole("button", { name: "Correct" });
    expect(run).toBeEnabled();
    await user.click(run);

    expect(
      screen.getByText("Waiting for the model to finish its current task…"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel this request" })).toBeInTheDocument();
    expect(correctStream).not.toHaveBeenCalled();

    await finishReply();

    expect(correctStream).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole("button", { name: "Correct" })).toBeInTheDocument();
    expect(screen.queryByText(/Waiting for the model/)).not.toBeInTheDocument();
  });

  it("can be cancelled while it waits, without reaching the model", async () => {
    const finishReply = replyRunning();
    const user = userEvent.setup();
    render(<WritingToolsScreen mode="correct" />);

    await user.type(screen.getByLabelText("Your text"), "fixd text");
    await user.click(screen.getByRole("button", { name: "Correct" }));
    await user.click(screen.getByRole("button", { name: "Cancel this request" }));

    expect(await screen.findByRole("button", { name: "Correct" })).toBeEnabled();
    expect(stopGeneration).not.toHaveBeenCalled();
    await finishReply();
    expect(correctStream).not.toHaveBeenCalled();
  });
});
