import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ImproveView from "./ImproveView";
import { resetAlertTracking, useStore } from "../state/store";
import type { ChatEvent, EnhanceOutcome, Settings } from "../lib/types";

const { enhancePromptStream, updateSettings, stopGeneration } = vi.hoisted(() => ({
  enhancePromptStream: vi.fn(),
  updateSettings: vi.fn().mockResolvedValue(undefined),
  stopGeneration: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/api", () => ({ api: { enhancePromptStream, updateSettings, stopGeneration } }));

/** A streamed improve that stays open until `release()` is called: the
 * caller can inspect the partial UI (working indicator, preview) before the
 * invoke's own promise resolves with the cleaned outcome, exactly like
 * `enhance_prompt_stream`. */
function streamingMock(finalText: string) {
  let release!: () => void;
  const held = new Promise<void>((resolve) => (release = resolve));
  enhancePromptStream.mockImplementation(
    async (
      _text: string,
      _tone: string,
      _instructions: string,
      onEvent: (e: ChatEvent) => void,
    ): Promise<EnhanceOutcome> => {
      onEvent({ type: "token", data: { text: finalText } });
      await held;
      return { type: "enhanced", data: { text: finalText } };
    },
  );
  return async () => {
    await act(async () => {
      release();
    });
  };
}

beforeEach(() => {
  resetAlertTracking();
  useStore.setState({
    modelRequests: [],
    streaming: false,
    settings: { improveToneInstructions: "" } as unknown as Settings,
  });
  enhancePromptStream.mockReset();
  enhancePromptStream.mockResolvedValue({ type: "enhanced", data: { text: "A clearer message." } });
  updateSettings.mockClear();
  stopGeneration.mockClear();
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
    expect(enhancePromptStream).not.toHaveBeenCalled();

    finishReply();
    await act(async () => {
      await reply;
    });

    // Natural is the default tone, and no custom instructions are sent.
    expect(enhancePromptStream).toHaveBeenCalledWith(
      "make this better pls",
      "natural",
      "",
      expect.any(Function),
    );
    expect(await screen.findByDisplayValue("A clearer message.")).toBeInTheDocument();
  });
});

describe("ImproveView streaming", () => {
  it("shows the working indicator, then the answer as it arrives", async () => {
    // Only reveal the first token: the answer must already be on screen
    // before the invoke resolves.
    let release!: () => void;
    const held = new Promise<void>((resolve) => (release = resolve));
    enhancePromptStream.mockImplementation(
      async (_t, _tone, _i, onEvent: (e: ChatEvent) => void) => {
        onEvent({ type: "token", data: { text: "A clearer " } });
        await held;
        onEvent({ type: "token", data: { text: "message." } });
        return { type: "enhanced", data: { text: "A clearer message." } };
      },
    );

    const user = userEvent.setup();
    render(<ImproveView />);
    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("button", { name: "Improve" }));

    expect(await screen.findByText("Improving…")).toBeInTheDocument();
    expect(screen.getByDisplayValue("A clearer")).toBeInTheDocument();

    await act(async () => {
      release();
    });
    expect(await screen.findByDisplayValue("A clearer message.")).toBeInTheDocument();
    expect(screen.queryByText("Improving…")).not.toBeInTheDocument();
  });

  it("keeps the partial rewrite on screen and shows the stopped notice", async () => {
    let finishStream!: () => void;
    enhancePromptStream.mockImplementation(
      async (_t, _tone, _i, onEvent: (e: ChatEvent) => void) => {
        onEvent({ type: "token", data: { text: "A clearer" } });
        await new Promise<void>((resolve) => (finishStream = resolve));
        return { type: "kept", data: { reason: "cancelled" } } as EnhanceOutcome;
      },
    );
    stopGeneration.mockImplementation(async () => finishStream());

    const user = userEvent.setup();
    render(<ImproveView />);
    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("button", { name: "Improve" }));
    await user.click(await screen.findByRole("button", { name: "Stop" }));

    expect(await screen.findByDisplayValue("A clearer")).toBeInTheDocument();
    expect(
      await screen.findByText("Stopped. Your message is unchanged."),
    ).toBeInTheDocument();
  });
});

describe("ImproveView tone picker", () => {
  it("sends exactly one selected tone, Natural by default", async () => {
    const release = streamingMock("A clearer message.");
    const user = userEvent.setup();
    render(<ImproveView />);

    const natural = screen.getByRole("radio", { name: "Natural" });
    expect(natural).toBeChecked();

    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("radio", { name: "Professional" }));
    await user.click(screen.getByRole("button", { name: "Improve" }));
    await release();
    await screen.findByDisplayValue("A clearer message.");

    expect(enhancePromptStream).toHaveBeenCalledWith(
      "make this better pls",
      "professional",
      "",
      expect.any(Function),
    );
    expect(screen.getByRole("radio", { name: "Professional" })).toBeChecked();
    expect(natural).not.toBeChecked();
  });

  it("reveals one instructions field for Custom and persists it on blur", async () => {
    const release = streamingMock("Warmer and shorter.");
    const user = userEvent.setup();
    render(<ImproveView />);

    expect(screen.queryByLabelText("Your instructions")).not.toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "Custom" }));
    const field = screen.getByLabelText("Your instructions");
    expect(field).toHaveValue("");
    // Same cap as `prompt_enhancer::MAX_TONE_INSTRUCTIONS_CHARS`.
    expect(field).toHaveAttribute("maxlength", "500");

    await user.type(field, "Make it warmer and shorter.");
    await user.tab(); // blur persists

    await waitFor(() =>
      expect(updateSettings).toHaveBeenCalledWith(
        expect.objectContaining({ improveToneInstructions: "Make it warmer and shorter." }),
      ),
    );

    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("button", { name: "Improve" }));
    await release();

    expect(enhancePromptStream).toHaveBeenCalledWith(
      "make this better pls",
      "custom",
      "Make it warmer and shorter.",
      expect.any(Function),
    );
  });

  it("seeds the instructions field from the saved setting", async () => {
    useStore.setState({
      settings: { improveToneInstructions: "Keep it short." } as unknown as Settings,
    });
    const user = userEvent.setup();
    render(<ImproveView />);
    await user.click(screen.getByRole("radio", { name: "Custom" }));
    expect(screen.getByLabelText("Your instructions")).toHaveValue("Keep it short.");
  });
});

describe("ImproveView actions", () => {
  it("gives every icon action a hover label and the same accessible name", async () => {
    const release = streamingMock("A clearer message.");
    const user = userEvent.setup();
    render(<ImproveView />);
    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("button", { name: "Improve" }));
    await release();
    await screen.findByDisplayValue("A clearer message.");

    for (const name of ["Copy", "Start a new chat with this", "Clear"]) {
      const button = screen.getByRole("button", { name });
      expect(button).toHaveAttribute("title", name);
    }
  });

  it("Clear empties the draft, the result and the status", async () => {
    const release = streamingMock("A clearer message.");
    const user = userEvent.setup();
    render(<ImproveView />);
    await user.type(screen.getByLabelText("Your message"), "make this better pls");
    await user.click(screen.getByRole("button", { name: "Improve" }));
    await release();
    await screen.findByDisplayValue("A clearer message.");

    await user.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByLabelText("Your message")).toHaveValue("");
    expect(screen.getByLabelText("Improved message")).toHaveValue("");
    expect(screen.queryByRole("button", { name: "Copy" })).not.toBeInTheDocument();
  });

  it("shows the page title with a decorative icon", () => {
    render(<ImproveView />);
    const heading = screen.getByRole("heading", { name: "Improve a message" });
    expect(heading.querySelector("svg[aria-hidden='true']")).not.toBeNull();
  });
});
