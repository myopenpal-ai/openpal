import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ChatView from "./ChatView";
import { useStore } from "../state/store";
import type { Conversation, EngineStatus, ModelInfo, Settings } from "../lib/types";
import { DEFAULT_SAMPLING } from "../lib/types";

const RUNNING = {
  type: "running",
  data: { modelId: "m", port: 1, diskSlow: false, autoUnloadAt: null },
} as const;

// The backend reports whatever engine state the test last put in the store,
// so a status poll (typing's idle-timer nudge triggers one) never flips it.
vi.mock("../lib/api", () => ({
  api: new Proxy(
    {},
    {
      get: (_api, name) =>
        name === "getEngineStatus"
          ? vi.fn(async () => useStore.getState().engine)
          : vi.fn().mockResolvedValue(null),
    },
  ),
}));

const conversation = {
  id: "c",
  title: "Chat c",
  createdAtMs: 0,
  updatedAtMs: 0,
  messages: [],
  sampling: DEFAULT_SAMPLING,
  contextLength: 4096,
  taskType: "general",
  branches: {},
  pinned: false,
} as unknown as Conversation;

const installed = {
  spec: { id: "m", name: "Model M" },
  status: { type: "installed", data: { path: "/models/m.gguf", sizeBytes: 1 } },
} as unknown as ModelInfo;

const useModel = vi.fn(async () => {});

function show(settings: Partial<Settings> = {}, engine: EngineStatus = RUNNING) {
  useStore.setState({
    engine,
    settings: {
      autoUnloadEnabled: true,
      chatBackend: "local",
      selectedModelId: "m",
      ...settings,
    } as Settings,
  });
  render(<ChatView />);
  return { user: userEvent.setup(), box: screen.getByRole("textbox") };
}

async function unloadByHand() {
  await act(() => useStore.getState().stopModel());
}

beforeEach(() => {
  useModel.mockClear();
  useStore.setState({
    active: conversation,
    modelRequests: [],
    streaming: false,
    engineBusy: false,
    userStoppedModel: false,
    plugins: [],
    models: [installed],
    skills: [],
    chatError: null,
    liveActivity: null,
    pendingToolConsent: null,
    pendingClarifyingQuestion: null,
    pendingInjectionReview: null,
    safetyAlerts: {},
    useModel,
  });
});

describe("typing in the composer with smart load/unload on", () => {
  it("loads the model after it was unloaded by hand with an empty composer", async () => {
    const { user, box } = show();
    await unloadByHand();

    await user.type(box, "hello");

    expect(useModel).toHaveBeenCalledWith("m");
  });

  it("loads the model after an idle auto-unload", async () => {
    const { user, box } = show({}, { type: "stopped" });

    await user.type(box, "hello");

    expect(useModel).toHaveBeenCalledWith("m");
  });

  it("leaves an unload made mid-draft alone until a fresh draft starts", async () => {
    const { user, box } = show();
    await user.type(box, "hel");
    await unloadByHand();

    await user.type(box, "lo there");
    expect(useModel).not.toHaveBeenCalled();

    await user.clear(box);
    await user.type(box, "new message");
    expect(useModel).toHaveBeenCalledWith("m");
  });
});

describe("typing in the composer never loads the model", () => {
  it("with smart load/unload off", async () => {
    const { user, box } = show({ autoUnloadEnabled: false });
    await unloadByHand();

    await user.type(box, "hello");

    expect(useModel).not.toHaveBeenCalled();
  });

  it("with the cloud backend", async () => {
    const { user, box } = show({ chatBackend: "cloud" }, { type: "stopped" });

    await user.type(box, "hello");

    expect(useModel).not.toHaveBeenCalled();
  });
});

describe("the model quick picker while a model is loading", () => {
  const STARTING = {
    type: "starting",
    data: { modelId: "m", progressPercent: null },
  } as const;

  it("keeps every option usable mid-load, so picking another model redirects the load", async () => {
    const second = {
      spec: { id: "n", name: "Model N" },
      status: { type: "installed", data: { path: "/models/n.gguf", sizeBytes: 1 } },
    } as unknown as ModelInfo;
    useStore.setState({ models: [installed, second] });
    const { user } = show({}, STARTING);
    useStore.setState({ engineBusy: true }); // a load is in flight

    await user.click(screen.getByRole("button", { name: "Switch model" }));

    const ownRow = screen.getByRole("option", { name: /Model M/ });
    const otherRow = screen.getByRole("option", { name: /Model N/ });
    // Both stay clickable mid-load — the disabled={engineBusy} that froze
    // them is gone, so a long load can be redirected.
    expect(ownRow).toBeEnabled();
    expect(otherRow).toBeEnabled();

    await user.click(otherRow);
    expect(useModel).toHaveBeenCalledWith("n");
    // The menu closes after picking.
    expect(screen.queryByRole("option")).toBeNull();
  });
});
