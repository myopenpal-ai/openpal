import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ImproveView from "./ImproveView";
import { resetAlertTracking, useStore } from "../state/store";

const { enhancePrompt } = vi.hoisted(() => ({ enhancePrompt: vi.fn() }));

vi.mock("../lib/api", () => ({ api: { enhancePrompt } }));

beforeEach(() => {
  resetAlertTracking();
  useStore.setState({ modelRequests: [], streaming: false });
  enhancePrompt.mockReset();
  enhancePrompt.mockResolvedValue({ type: "enhanced", data: { text: "A clearer message." } });
});

describe("ImproveView while a chat reply is running", () => {
  it("waits for it, shown as waiting, then runs", async () => {
    // A reply already using the model, at the head of the queue.
    let finishReply!: () => void;
    const reply = useStore
      .getState()
      .runModelRequest(
        { id: "reply", kind: "writingTool" },
        () => new Promise<void>((resolve) => (finishReply = resolve)),
      );
    const user = userEvent.setup();
    render(<ImproveView />);

    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    const run = screen.getByRole("button", { name: "Improve" });
    expect(run).toBeEnabled();
    await user.click(run);

    expect(
      screen.getByText("Waiting for the model to finish its current task…"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel this request" })).toBeInTheDocument();
    expect(enhancePrompt).not.toHaveBeenCalled();

    finishReply();
    await act(async () => {
      await reply;
    });

    expect(enhancePrompt).toHaveBeenCalledWith("make this better pls");
    expect(await screen.findByDisplayValue("A clearer message.")).toBeInTheDocument();
  });
});
