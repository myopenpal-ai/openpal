import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import WritingToolsScreen from "./WritingToolsScreen";
import { resetAlertTracking, useStore } from "../state/store";
import type { ChatEvent } from "../lib/types";

const { correctStream, translateStream, stopGeneration, copyText, saveWritingToolResult, pickAttachments } =
  vi.hoisted(() => ({
    correctStream: vi.fn(),
    translateStream: vi.fn(),
    stopGeneration: vi.fn().mockResolvedValue(undefined),
    copyText: vi.fn().mockResolvedValue(undefined),
    saveWritingToolResult: vi.fn().mockResolvedValue(true),
    pickAttachments: vi.fn().mockResolvedValue([]),
  }));

vi.mock("../lib/api", () => ({
  api: { correctStream, translateStream, stopGeneration, copyText, saveWritingToolResult, pickAttachments },
}));

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

/** A correction that arrives in one token, as `correct_stream` would send it. */
function correctionOf(text: string) {
  correctStream.mockImplementation(async (_text: string, onEvent: (e: ChatEvent) => void) => {
    onEvent({ type: "token", data: { text } });
  });
}

async function correct(user: ReturnType<typeof userEvent.setup>, source: string, corrected: string) {
  correctionOf(corrected);
  render(<WritingToolsScreen mode="correct" />);
  await user.type(screen.getByLabelText("Your text"), source);
  await user.click(screen.getByRole("button", { name: "Correct" }));
  await screen.findAllByRole("button", { name: /^(Apply|Remove): / });
}

beforeEach(() => {
  resetAlertTracking();
  useStore.setState({ modelRequests: [], streaming: false });
  stopGeneration.mockClear();
  copyText.mockClear();
  saveWritingToolResult.mockClear();
  pickAttachments.mockReset();
  pickAttachments.mockResolvedValue([]);
  correctStream.mockReset();
  translateStream.mockReset();
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

describe("Correct mode — one editable surface with marks", () => {
  it("shows the user's own text as the only textbox, with no diff pane", async () => {
    const user = userEvent.setup();
    await correct(user, "The cat sat on the mat.", "The dog sat on the rug.");

    expect(screen.getAllByRole("textbox")).toHaveLength(1);
    expect(screen.getByLabelText("Your text")).toHaveValue("The cat sat on the mat.");
    expect(screen.queryByText("Result")).not.toBeInTheDocument();
  });

  it("groups corrections separated only by whitespace into one suggestion", async () => {
    const user = userEvent.setup();
    await correct(user, "Thanks, you'r nise", "Thanks, you're nice");

    const marks = screen.getAllByRole("button", { name: /^Apply: / });
    expect(marks).toHaveLength(1);
    expect(marks[0]).toHaveAccessibleName("Apply: re nice");
  });

  it("shows the suggestion and its click-to-apply hint on hover", async () => {
    const user = userEvent.setup();
    await correct(user, "The cat sat on the mat.", "The dog sat on the rug.");

    const mark = screen.getByRole("button", { name: "Apply: dog" });
    await user.hover(mark);

    // The tooltip opens on hover, with the instruction under the suggestion,
    // and the text itself is still untouched until the mark is clicked.
    expect(screen.getByText("dog")).toBeInTheDocument();
    expect(screen.getByText("Click to apply")).toBeInTheDocument();
    expect(screen.getByLabelText("Your text")).toHaveValue("The cat sat on the mat.");

    await user.unhover(mark);
    expect(screen.queryByText("Click to apply")).not.toBeInTheDocument();
  });

  it("writes a suggestion in on click and keeps the surface editable", async () => {
    const user = userEvent.setup();
    await correct(user, "The cat sat on the mat.", "The dog sat on the rug.");

    const surface = screen.getByLabelText("Your text");
    await user.click(screen.getByRole("button", { name: "Apply: dog" }));

    // The pre-correction wording is gone — this is a destructive apply.
    expect(surface).toHaveValue("The dog sat on the mat.");
    expect(screen.queryByRole("button", { name: "Apply: dog" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Apply: rug" })).toBeInTheDocument();
    // Still editable after a suggestion lands.
    expect(surface).not.toBeDisabled();
  });

  it("applies a suggestion from the keyboard alone, then hands focus on", async () => {
    const user = userEvent.setup();
    await correct(user, "The cat sat on the mat.", "The dog sat on the rug.");

    const mark = screen.getByRole("button", { name: "Apply: dog" });
    mark.focus();
    expect(mark).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.getByLabelText("Your text")).toHaveValue("The dog sat on the mat.");

    // Applying unmounted that mark: focus moves to the next suggestion
    // instead of falling back to the document.
    const next = screen.getByRole("button", { name: "Apply: rug" });
    expect(next).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.getByLabelText("Your text")).toHaveValue("The dog sat on the rug.");
    // After the last suggestion, focus lands in the surface for the next edit.
    expect(screen.getByLabelText("Your text")).toHaveFocus();
  });

  it("Keep all writes every suggestion in; Discard all leaves the text alone", async () => {
    const user = userEvent.setup();
    await correct(user, "The cat sat on the mat.", "The dog sat on the rug.");
    await user.click(screen.getByRole("button", { name: "Keep all" }));
    expect(screen.getByLabelText("Your text")).toHaveValue("The dog sat on the rug.");
    expect(screen.queryByRole("button", { name: /^(Apply|Remove): / })).not.toBeInTheDocument();

    // A fresh correction, discarded in full, keeps the original wording.
    correctionOf("The dog sits on the rug.");
    await user.click(screen.getByRole("button", { name: "Correct" }));
    await screen.findByRole("button", { name: "Discard all" });
    expect(screen.getByRole("button", { name: "Apply: sits" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Discard all" }));
    expect(screen.getByLabelText("Your text")).toHaveValue("The dog sat on the rug.");
    expect(screen.queryByRole("button", { name: /^(Apply|Remove): / })).not.toBeInTheDocument();
  });

  it("Copy and Download act on the text left after the choices", async () => {
    pickAttachments.mockResolvedValue([
      { kind: "document", name: "notes.txt", text: "The cat sat on the mat.", mimeType: "text/plain" },
    ]);
    correctionOf("The dog sat on the mat.");
    const user = userEvent.setup();
    render(<WritingToolsScreen mode="correct" />);
    await user.click(screen.getByRole("button", { name: "Upload a document" }));
    await screen.findByDisplayValue("The cat sat on the mat.");

    await user.click(screen.getByRole("button", { name: "Correct" }));
    await user.click(await screen.findByRole("button", { name: "Apply: dog" }));

    await user.click(screen.getByRole("button", { name: "Copy" }));
    expect(copyText).toHaveBeenCalledWith("The dog sat on the mat.");

    await user.click(screen.getByRole("button", { name: "Download" }));
    expect(saveWritingToolResult).toHaveBeenCalledWith(
      "The dog sat on the mat.",
      "notes (corrected).txt",
    );
  });
});

describe("Correct mode — working indicator and Stop", () => {
  it("shows the streamed correction as it arrives, then turns it into marks", async () => {
    let release!: () => void;
    correctStream.mockImplementation(async (_text: string, onEvent: (e: ChatEvent) => void) => {
      onEvent({ type: "token", data: { text: "The dog " } });
      await new Promise<void>((resolve) => (release = resolve));
      onEvent({ type: "token", data: { text: "sat on the mat." } });
    });

    const user = userEvent.setup();
    render(<WritingToolsScreen mode="correct" />);
    await user.type(screen.getByLabelText("Your text"), "The cat sat on the mat.");
    await user.click(screen.getByRole("button", { name: "Correct" }));

    expect(await screen.findByText("Correcting…")).toBeInTheDocument();
    expect(screen.getByText("The dog")).toBeInTheDocument();

    await act(async () => {
      release();
    });
    expect(await screen.findByRole("button", { name: "Apply: dog" })).toBeInTheDocument();
    expect(screen.queryByText("Correcting…")).not.toBeInTheDocument();
  });

  it("leaves the partial rewrite on screen and shows the stopped notice", async () => {
    let finishStream!: () => void;
    correctStream.mockImplementation(async (_text: string, onEvent: (e: ChatEvent) => void) => {
      onEvent({ type: "token", data: { text: "The dog sat" } });
      await new Promise<void>((resolve) => (finishStream = resolve));
    });
    stopGeneration.mockImplementation(async () => finishStream());

    const user = userEvent.setup();
    render(<WritingToolsScreen mode="correct" />);
    await user.type(screen.getByLabelText("Your text"), "The cat sat on the mat.");
    await user.click(screen.getByRole("button", { name: "Correct" }));
    await screen.findByText("Correcting…");
    await user.click(screen.getByRole("button", { name: "Stop" }));

    expect(await screen.findByText("The dog sat")).toBeInTheDocument();
    expect(
      await screen.findByText("Stopped. Your message is unchanged."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^(Apply|Remove): / })).not.toBeInTheDocument();
  });
});

describe("Translate mode", () => {
  it("streams the translation progressively with a working indicator", async () => {
    let release!: () => void;
    translateStream.mockImplementation(
      async (
        _text: string,
        _source: string | null,
        _target: string,
        onEvent: (e: ChatEvent) => void,
      ) => {
        onEvent({ type: "token", data: { text: "Bon" } });
        await new Promise<void>((resolve) => (release = resolve));
        onEvent({ type: "token", data: { text: "jour" } });
      },
    );

    const user = userEvent.setup();
    render(<WritingToolsScreen mode="translate" />);
    await user.type(screen.getByLabelText("Your text"), "hello");
    await user.selectOptions(screen.getByLabelText("Translate into"), "fr");
    await user.click(screen.getByRole("button", { name: "Translate" }));

    expect(await screen.findByText("Translating…")).toBeInTheDocument();
    expect(screen.getByLabelText("Result")).toHaveValue("Bon");

    await act(async () => {
      release();
    });
    expect(await screen.findByDisplayValue("Bonjour")).toBeInTheDocument();
    expect(screen.queryByText("Translating…")).not.toBeInTheDocument();
  });

  it("offers icon actions with hover labels and accessible names, and Clear", async () => {
    translateStream.mockImplementation(
      async (
        _text: string,
        _source: string | null,
        _target: string,
        onEvent: (e: ChatEvent) => void,
      ) => {
        onEvent({ type: "token", data: { text: "Bonjour" } });
      },
    );

    const user = userEvent.setup();
    render(<WritingToolsScreen mode="translate" />);
    await user.type(screen.getByLabelText("Your text"), "hello");
    await user.selectOptions(screen.getByLabelText("Translate into"), "fr");
    await user.click(screen.getByRole("button", { name: "Translate" }));
    await screen.findByDisplayValue("Bonjour");

    for (const name of ["Copy", "Clear"]) {
      const button = screen.getByRole("button", { name });
      expect(button).toHaveAttribute("title", name);
    }

    await user.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByLabelText("Your text")).toHaveValue("");
    expect(screen.getByLabelText("Result")).toHaveValue("");
    expect(screen.queryByRole("button", { name: "Copy" })).not.toBeInTheDocument();
  });

  it("downloads an uploaded document's translation", async () => {
    pickAttachments.mockResolvedValue([
      { kind: "document", name: "notes.txt", text: "hello", mimeType: "text/plain" },
    ]);
    translateStream.mockImplementation(
      async (
        _text: string,
        _source: string | null,
        _target: string,
        onEvent: (e: ChatEvent) => void,
      ) => {
        onEvent({ type: "token", data: { text: "Bonjour" } });
      },
    );

    const user = userEvent.setup();
    render(<WritingToolsScreen mode="translate" />);
    await user.click(screen.getByRole("button", { name: "Upload a document" }));
    await screen.findByDisplayValue("hello");
    await user.selectOptions(screen.getByLabelText("Translate into"), "fr");
    await user.click(screen.getByRole("button", { name: "Translate" }));
    await screen.findByDisplayValue("Bonjour");

    const download = screen.getByRole("button", { name: "Download" });
    expect(download).toHaveAttribute("title", "Download");
    await user.click(download);
    expect(saveWritingToolResult).toHaveBeenCalledWith("Bonjour", "notes (French).txt");
  });

  it("shows the page title with its sidebar icon, decorative only", () => {
    render(<WritingToolsScreen mode="translate" />);
    const heading = screen.getByRole("heading", { name: "Translate" });
    expect(heading.querySelector("svg[aria-hidden='true']")).not.toBeNull();
  });
});

describe("Correct mode — page title", () => {
  it("carries a decorative icon rather than an extra label", () => {
    render(<WritingToolsScreen mode="correct" />);
    const heading = screen.getByRole("heading", { name: "Correct" });
    expect(heading.querySelector("svg[aria-hidden='true']")).not.toBeNull();
  });
});
