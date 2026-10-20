import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "../i18n";
import Onboarding from "./Onboarding";
import { useStore } from "../state/store";
import type {
  AppInfo,
  DownloadEvent,
  EngineStatus,
  HardwareInfo,
  ModelInfo,
  ModelSpec,
  Settings,
  SystemReport,
} from "../lib/types";

const {
  downloadModel,
  cancelDownload,
  getSettings,
  getModels,
  updateSettings,
  notificationPermission,
  requestNotificationPermission,
  installMockDefaults,
} = vi.hoisted(() => {
  const downloadModel = vi.fn();
  const cancelDownload = vi.fn();
  const getSettings = vi.fn();
  const getModels = vi.fn();
  const updateSettings = vi.fn();
  // The OS permission behind the notifications step: turning them on asks
  // (`requestNotificationPermission`) once; nothing here has a reason to merely
  // read it (`notificationPermission`).
  const notificationPermission = vi.fn();
  const requestNotificationPermission = vi.fn();
  // Re-installed after a full reset before every test, so a `…Once` that an
  // earlier (failing) test left unconsumed can't leak into the next one.
  const installMockDefaults = () => {
    // Emits the `started` Channel event the store listens for, then never
    // settles — the mocked-tier equivalent of a download genuinely in flight
    // (jsdom has no Channel wire, and `downloads` is only cleared when the
    // promise settles). Tests override this to simulate "nothing downloading".
    downloadModel.mockReset().mockImplementation(
      (_modelId: string, onEvent: (e: DownloadEvent) => void) => {
        onEvent({ type: "started", data: { totalBytes: 100, resumedFromBytes: 1 } });
        return new Promise<void>(() => {});
      },
    );
    cancelDownload.mockReset().mockResolvedValue(undefined);
    getSettings.mockReset().mockResolvedValue({ onboardingCompleted: false });
    // `refreshModels` runs after a download failure / completion; return the
    // store's current list so the recommended model stays mounted.
    getModels.mockReset().mockImplementation(async () => useStore.getState().models);
    updateSettings.mockReset().mockResolvedValue(undefined);
    notificationPermission.mockReset().mockResolvedValue("granted");
    requestNotificationPermission.mockReset().mockResolvedValue("granted");
  };
  installMockDefaults();
  return {
    downloadModel,
    cancelDownload,
    getSettings,
    getModels,
    updateSettings,
    notificationPermission,
    requestNotificationPermission,
    installMockDefaults,
  };
});

vi.mock("../lib/api", () => ({
  api: {
    downloadModel,
    cancelDownload,
    getSettings,
    getModels,
    updateSettings,
    notificationPermission,
    requestNotificationPermission,
  },
}));

const SPEC: ModelSpec = {
  id: "qwen3-4b",
  name: "Qwen 3 4B",
  family: "Qwen",
  publisher: "Qwen",
  description: "A test model.",
  strengths: [],
  url: "https://example.com/qwen3-4b.gguf",
  fileName: "qwen3-4b.gguf",
  sizeBytes: 4 * 1024 ** 3,
  quant: "Q4_K_M",
  sha256: null,
  paramsB: 4,
  moe: false,
  contextMax: 32768,
  qualityScore: 50,
  license: "Apache 2.0",
  extraArgs: [],
  hidden: false,
  curated: false,
  source: "catalog",
  localPath: null,
  vision: false,
  taskScores: {},
  releaseDate: "2025-01-01",
};

const MODEL: ModelInfo = {
  spec: SPEC,
  status: { type: "notInstalled" },
  fit: "perfect",
  why: "Fits easily in your memory.",
  isRecommended: true,
  favorite: false,
};

// A second model, so a test can pick something other than the recommendation.
const ALT: ModelInfo = {
  ...MODEL,
  spec: { ...SPEC, id: "alt-model", name: "Alt Model 1B", fileName: "alt.gguf", sizeBytes: 1024 ** 3 },
  fit: "good",
  why: "Fits fine.",
  isRecommended: false,
};

const HARDWARE: HardwareInfo = {
  chip: "Apple M2",
  isAppleSilicon: true,
  totalRamBytes: 16 * 1024 ** 3,
  availableRamBytes: 12 * 1024 ** 3,
  cpuCores: 8,
  hasMetal: true,
  freeDiskBytes: 500 * 1024 ** 3,
  osVersion: "14.0",
};

const RUNNING: EngineStatus = {
  type: "running",
  data: { modelId: "qwen3-4b", port: 1, diskSlow: false, autoUnloadAt: null },
};
const STARTING: EngineStatus = {
  type: "starting",
  data: { modelId: "qwen3-4b", progressPercent: null },
};
const FAILED_ENGINE: EngineStatus = { type: "failed", data: { message: "boom" } };

/** The model finishing its download: installed, nothing in flight any more. */
const INSTALLED: ModelInfo = {
  ...MODEL,
  status: { type: "installed", data: { path: "/m/qwen3-4b.gguf", sizeBytes: 1 } },
};

/** Onboarding primed to recommend qwen3-4b (not installed). */
function seedNoDownload() {
  useStore.setState({
    models: [MODEL],
    report: {
      hardware: HARDWARE,
      recommendation: {
        primaryModelId: "qwen3-4b",
        headline: "Qwen 3 4B is the best fit for your Mac.",
        reasons: ["Fits easily in your memory."],
        ranked: [{ modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." }],
      },
    } as SystemReport,
    settings: { autoLoadOnStartup: true, defaultModelId: null } as unknown as Settings,
    engine: { type: "stopped" },
    downloads: {},
    downloadErrors: {},
    notice: null,
    appInfo: null,
    notificationPermission: "granted",
  });
}

const click = (name: string | RegExp) =>
  userEvent.click(screen.getByRole("button", { name }));

const skipStep = () => click("Skip this step");

/** An option of the appearance step, by its name. */
const radio = (name: string) => screen.getByRole("radio", { name });

/** Picks an option of the appearance step by clicking its radio. */
const pick = (name: string) => userEvent.click(radio(name));

/** The "Skip setup?" confirmation that skipping the whole setup goes through. */
const skipDialog = () => screen.getByRole("alertdialog", { name: "Skip setup?" });
const noDialog = () => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();

/** Opens the confirmation from whichever scene is showing (its "Skip setup" button). */
const openSkipSetup = () => click("Skip setup");

async function confirmSkip() {
  await userEvent.click(within(skipDialog()).getByRole("button", { name: "Skip setup" }));
}

const completedSaves = () =>
  updateSettings.mock.calls.filter(([s]) => s?.onboardingCompleted === true);

/** The `theme` of every settings write that carries one, in the order they were
 * sent. The seeded settings have none, so a write only shows up here if the
 * appearance step put a theme in it. */
const themesWritten = () =>
  updateSettings.mock.calls.flatMap(([s]) => (s?.theme === undefined ? [] : [s.theme]));

// Drivers, each starting from a fresh render on the welcome scene.
// The scan title holds a non-breaking space, which `getByRole`'s `name` doesn't
// normalize the way Playwright's does — hence the regex.
const SCAN_HEADING = /^First, let's see what your Mac can\sdo\.$/;
const APPEARANCE_HEADING = "How should OpenPal look?";
const NOTIFICATIONS_HEADING = "Want a heads-up?";
async function toName() {
  await click("Let's set things up");
  await screen.findByRole("heading", { name: "What should we call you?" });
}
async function toAppearance() {
  await toName();
  await skipStep(); // the name step's own skip
  // Waiting here matters: the name step's skip is async (it awaits its save), so a
  // click made before the new scene is up would land on the name step's still-mounted,
  // disabled button and silently do nothing.
  await screen.findByRole("heading", { name: APPEARANCE_HEADING });
}
async function toNotifications() {
  await toAppearance();
  await skipStep(); // the appearance step's skip: nothing picked, nothing saved
  await screen.findByRole("heading", { name: NOTIFICATIONS_HEADING });
}
async function toScan() {
  await toNotifications();
  await skipStep(); // the notifications step's skip: left off, nothing saved
  await screen.findByRole("heading", { name: SCAN_HEADING });
}
async function toRecommend() {
  await toScan();
  await click("Show my match");
  await screen.findByRole("heading", { name: "Qwen 3 4B" });
}
async function toDownload() {
  await toRecommend();
  await click(/^Download/);
  await screen.findByRole("heading", { name: "Bringing Qwen 3 4B home…" });
}
async function toSecurity() {
  await toScan();
  await skipStep(); // skips the model setup
  await screen.findByRole("heading", { name: "Add a PIN?" });
}
async function toReady() {
  await toSecurity();
  await skipStep();
  // "Almost there." with no model, "All set." once one is on its way.
  await screen.findByRole("heading", { name: /^(All set|Almost there)\.$/ });
}

const MSG_NO_MODEL =
  "You can't chat until you pick a model, but you can do that anytime in Models.";
const MSG_DOWNLOADING =
  "Your model download will stop. You can resume it anytime in Models, but you can't chat until a model is ready.";
const MSG_MODEL_READY =
  "Your model is installed. All that's left is an optional PIN and the update check. You can find both in Settings later.";
const FINAL_NO_MODEL =
  "Pick a model in Models whenever you're ready, and you can start chatting. Once it's downloaded, it runs on your Mac, even with the internet off.";
const FINAL_DOWNLOAD_STOPPED =
  "Your download was stopped. Resume it in Models whenever you're ready, and you can start chatting.";

beforeEach(() => {
  installMockDefaults();
  // A pick pins the theme on <html>, and jsdom keeps the document between tests.
  document.documentElement.removeAttribute("data-theme");
  seedNoDownload();
});

describe("Onboarding: the two skip buttons", () => {
  it("the welcome screen offers only Skip setup: its main button already moves on", () => {
    render(<Onboarding />);

    expect(screen.getByRole("button", { name: "Skip setup" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Skip this step" })).not.toBeInTheDocument();
  });

  it("the name screen's Skip this step moves on without asking and saves no name", async () => {
    render(<Onboarding />);
    await toName();

    await skipStep();

    expect(
      await screen.findByRole("heading", { name: APPEARANCE_HEADING }),
    ).toBeInTheDocument();
    noDialog();
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: null }));
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it.each([
    ["name", toName],
    ["appearance", toAppearance],
    ["notifications", toNotifications],
    ["scan", toScan],
    ["recommend", toRecommend],
  ])("the %s screen's Skip setup asks first", async (_scene, drive) => {
    render(<Onboarding />);
    await drive();

    await openSkipSetup();

    expect(skipDialog()).toBeInTheDocument();
    expect(updateSettings).not.toHaveBeenCalledWith(
      expect.objectContaining({ onboardingCompleted: true }),
    );
  });

  it.each([
    ["scan", toScan],
    ["recommend", toRecommend],
  ])(
    "the %s screen's Skip this step skips the model setup and lands on the PIN offer",
    async (_scene, drive) => {
      render(<Onboarding />);
      await drive();

      await skipStep();

      expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
      noDialog();
      expect(cancelDownload).not.toHaveBeenCalled();
      expect(downloadModel).not.toHaveBeenCalled();
      expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
    },
  );

  it("the download screen's Skip this step drops the download in flight and lands on the PIN offer", async () => {
    render(<Onboarding />);
    await toDownload();

    await skipStep();

    expect(cancelDownload).toHaveBeenCalledWith("qwen3-4b");
    expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
    noDialog();
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it("the download screen's Skip this step cancels nothing when no download is in flight", async () => {
    // A failed download: the store clears its in-flight entry on rejection. `Once`,
    // so the rejection can't leak into whichever test runs next.
    downloadModel.mockRejectedValueOnce(new Error("download failed"));
    render(<Onboarding />);
    await toRecommend();
    await click(/^Download/);
    await screen.findByRole("heading", { name: "Hmm, that didn't finish." });

    await skipStep();

    expect(cancelDownload).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
  });

  it("skipping the model setup cancels the model the user actually picked, not the recommended one", async () => {
    useStore.setState({
      models: [MODEL, ALT],
      report: {
        hardware: HARDWARE,
        recommendation: {
          primaryModelId: "qwen3-4b",
          headline: "Qwen 3 4B is the best fit for your Mac.",
          reasons: ["Fits easily in your memory."],
          ranked: [
            { modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." },
            { modelId: "alt-model", fit: "good", why: "Fits fine." },
          ],
        },
      } as SystemReport,
    });
    render(<Onboarding />);
    await toRecommend();
    await click("Pick a different one");
    await click(/Alt Model 1B/);
    await click(/^Download/);
    await screen.findByRole("heading", { name: "Bringing Alt Model 1B home…" });

    await skipStep();

    expect(cancelDownload).toHaveBeenCalledTimes(1);
    expect(cancelDownload).toHaveBeenCalledWith("alt-model");
  });

  it("an installed model with a stale progress entry is not 'a download in flight'", async () => {
    useStore.setState({
      models: [INSTALLED],
      downloads: { "qwen3-4b": { receivedBytes: 1, totalBytes: 100, bytesPerSec: 0, etaSeconds: 0 } },
    });
    render(<Onboarding />);

    await openSkipSetup();
    expect(within(skipDialog()).getByText(MSG_NO_MODEL)).toBeInTheDocument();
    await confirmSkip();

    expect(cancelDownload).not.toHaveBeenCalled();
  });

  it("the PIN offer's Skip this step moves on to the final screen without asking", async () => {
    render(<Onboarding />);
    await toSecurity();

    await skipStep();

    expect(await screen.findByRole("heading", { name: "Almost there." })).toBeInTheDocument();
    noDialog();
  });

  it("the PIN offer's Skip setup asks, and finishes without the final screen once confirmed", async () => {
    render(<Onboarding />);
    await toSecurity();

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(completedSaves()).toHaveLength(1));
    expect(useStore.getState().settings?.onboardingCompleted).toBe(true);
  });

  it("the name screen's skip buttons are inert while the name is saving", async () => {
    render(<Onboarding />);
    await toName();
    await userEvent.type(screen.getByLabelText("Your name"), "Ada");
    let finishSave: () => void = () => {};
    updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finishSave = resolve;
        }),
    );
    await click("Continue");

    expect(screen.getByRole("button", { name: "Skip this step" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Skip setup" })).toBeDisabled();

    finishSave();
    expect(await screen.findByRole("heading", { name: APPEARANCE_HEADING })).toBeInTheDocument();
    // The name step's saving state doesn't follow the user to the next screen.
    expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip this step" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip setup" })).toBeEnabled();
  });

  it("a half-typed name is discarded by Skip this step and never saved by Skip setup", async () => {
    render(<Onboarding />);
    await toName();
    await userEvent.type(screen.getByLabelText("Your name"), "Ada");

    await openSkipSetup();
    expect(skipDialog()).toBeInTheDocument();
    // Opening the dialog must not submit the form behind it.
    expect(updateSettings).not.toHaveBeenCalled();
    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));

    await skipStep();

    expect(await screen.findByRole("heading", { name: APPEARANCE_HEADING })).toBeInTheDocument();
    // Landing on the next screen saves nothing of its own.
    expect(updateSettings).toHaveBeenCalledTimes(1);
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: null }));
  });

  it("the scan screen offers both skips while the hardware report is still loading", async () => {
    useStore.setState({ report: null });
    render(<Onboarding />);
    await toScan();

    expect(screen.getByText("Taking a look…")).toBeInTheDocument();
    await openSkipSetup();
    expect(skipDialog()).toBeInTheDocument();
    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));

    await skipStep();

    expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
  });

  it("the download screen keeps both skips when the model won't load", async () => {
    useStore.setState({ models: [INSTALLED], engine: FAILED_ENGINE });
    render(<Onboarding />);
    await toRecommend();
    await click(/^Use /);
    await screen.findByRole("heading", { name: "It needs another nudge." });

    await openSkipSetup();
    expect(skipDialog()).toBeInTheDocument();
    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));
    await skipStep();

    expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
    expect(cancelDownload).not.toHaveBeenCalled();
  });

  it("the download screen keeps both skips while the model is waking up", async () => {
    useStore.setState({ models: [INSTALLED], engine: STARTING });
    render(<Onboarding />);
    await toRecommend();
    await click(/^Use /);
    await screen.findByRole("heading", { name: "Waking it up…" });

    await openSkipSetup();
    expect(within(skipDialog()).getByText(MSG_MODEL_READY)).toBeInTheDocument();
    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));
    await skipStep();

    expect(await screen.findByRole("heading", { name: "Add a PIN?" })).toBeInTheDocument();
    expect(cancelDownload).not.toHaveBeenCalled();
  });

  it("skipping the model setup also drops a restart queued behind a download that hasn't settled", async () => {
    let settleFirst: () => void = () => {};
    downloadModel.mockImplementationOnce(
      (_modelId: string, onEvent: (e: DownloadEvent) => void) => {
        onEvent({ type: "started", data: { totalBytes: 100, resumedFromBytes: 1 } });
        return new Promise<void>((resolve) => {
          settleFirst = resolve;
        });
      },
    );
    render(<Onboarding />);
    await toDownload();
    // Cancel goes back to the recommendation, but the cancelled download hasn't
    // settled yet — so pressing Download again only queues a restart behind it.
    await click("Cancel");
    await click(/^Download/);
    await screen.findByRole("heading", { name: "Bringing Qwen 3 4B home…" });

    await skipStep();
    await screen.findByRole("heading", { name: "Add a PIN?" });
    // The first download now settles: nothing may start after the user skipped.
    await act(async () => {
      settleFirst();
    });
    await waitFor(() => expect(useStore.getState().downloads).toEqual({}));

    expect(downloadModel).toHaveBeenCalledTimes(1);
  });
});

describe("Onboarding: the appearance step", () => {
  const html = document.documentElement;

  /** The saved settings with whatever `theme` the backend handed over (including
   * none at all, or one this version has never heard of). */
  function seedTheme(theme: unknown) {
    useStore.setState({
      settings: { ...useStore.getState().settings, theme } as unknown as Settings,
    });
  }

  it("is where the name step's Continue lands, once the name is saved", async () => {
    render(<Onboarding />);
    await toName();
    await userEvent.type(screen.getByLabelText("Your name"), "Ada");

    await click("Continue");

    expect(await screen.findByRole("heading", { name: APPEARANCE_HEADING })).toBeInTheDocument();
    expect(updateSettings).toHaveBeenCalledTimes(1);
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: "Ada" }));
  });

  it("offers System, Light and Dark, in that order, under a labelled and described group", async () => {
    render(<Onboarding />);
    await toAppearance();

    const group = screen.getByRole("radiogroup", { name: APPEARANCE_HEADING });
    expect(group).toHaveAccessibleDescription(
      "Pick a theme. You can change it anytime in Settings.",
    );
    const radios = within(group).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect(radios[0]).toHaveAccessibleName("Use system default");
    expect(radios[1]).toHaveAccessibleName("Light");
    expect(radios[2]).toHaveAccessibleName("Dark");
  });

  it("gives System's hint as its description, not as part of its name", async () => {
    render(<Onboarding />);
    await toAppearance();

    expect(radio("Use system default")).toHaveAccessibleDescription("Follows your device's light or dark mode");
    expect(radio("Light")).not.toHaveAccessibleDescription();
    expect(radio("Dark")).not.toHaveAccessibleDescription();
  });

  it("takes focus to the heading on arrival, not to an option", async () => {
    render(<Onboarding />);
    await toAppearance();

    // The scene-change effect runs just after the heading is in the DOM.
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: APPEARANCE_HEADING })).toHaveFocus(),
    );
  });

  it("can be continued from the first frame, and arriving saves nothing", async () => {
    render(<Onboarding />);
    await toAppearance();

    expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip this step" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip setup" })).toBeEnabled();
    // The one write so far is the name step's own skip; no theme went with it.
    expect(updateSettings).toHaveBeenCalledTimes(1);
    expect(themesWritten()).toEqual([]);
  });

  it.each([
    ["no theme saved yet", undefined],
    ["System saved", "system"],
    ["a theme this version doesn't know", "sepia"],
  ])("checks System when there is %s", async (_case, saved) => {
    seedTheme(saved);
    render(<Onboarding />);
    await toAppearance();

    expect(radio("Use system default")).toBeChecked();
    expect(radio("Light")).not.toBeChecked();
    expect(radio("Dark")).not.toBeChecked();
  });

  it.each([
    ["Light", "light"],
    ["Dark", "dark"],
  ])(
    "checks %s when that is the saved theme, as after quitting part-way through and coming back",
    async (name, saved) => {
      seedTheme(saved);
      render(<Onboarding />);
      await toAppearance();

      expect(radio(name)).toBeChecked();
      expect(radio("Use system default")).not.toBeChecked();
    },
  );

  it("re-themes at once on a pick and saves it there and then, with no Save step", async () => {
    render(<Onboarding />);
    await toAppearance();
    const heading = screen.getByRole("heading", { name: APPEARANCE_HEADING });

    await pick("Dark");

    expect(radio("Dark")).toBeChecked();
    expect(radio("Use system default")).not.toBeChecked();
    expect(useStore.getState().settings?.theme).toBe("dark");
    expect(html).toHaveAttribute("data-theme", "dark");
    expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "dark" }));
    // A pick doesn't leave the step, remount it, or take focus off the option.
    expect(screen.getByRole("heading", { name: APPEARANCE_HEADING })).toBe(heading);
    expect(radio("Dark")).toHaveFocus();
    noDialog();
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it("hands the theme back to the device when System is picked again", async () => {
    render(<Onboarding />);
    await toAppearance();
    await pick("Light");
    expect(html).toHaveAttribute("data-theme", "light");

    await pick("Use system default");

    expect(radio("Use system default")).toBeChecked();
    // No attribute at all: the palette then follows the OS by CSS alone, live.
    expect(html).not.toHaveAttribute("data-theme");
    expect(themesWritten()).toEqual(["light", "system"]);
  });

  it("ends on the last of several quick picks, whichever save finishes first", async () => {
    render(<Onboarding />);
    await toAppearance();
    const finishes: Array<() => void> = [];
    updateSettings.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finishes.push(resolve);
        }),
    );

    await pick("Light");
    await pick("Dark");

    // Both saves are still out, and the screen already shows the last pick.
    expect(radio("Dark")).toBeChecked();
    expect(html).toHaveAttribute("data-theme", "dark");
    // They come back in the opposite order: nothing may flip back to Light.
    await act(async () => {
      finishes[1]();
      finishes[0]();
    });

    expect(radio("Dark")).toBeChecked();
    expect(useStore.getState().settings?.theme).toBe("dark");
    expect(html).toHaveAttribute("data-theme", "dark");
    expect(themesWritten()).toEqual(["light", "dark"]);
  });

  it("takes a pick back on screen when it can't be saved", async () => {
    render(<Onboarding />);
    await toAppearance();
    updateSettings.mockRejectedValueOnce(new Error("disk full"));

    await pick("Dark");

    // The options follow the store, which drops a pick that never reached the disk.
    await waitFor(() => expect(radio("Use system default")).toBeChecked());
    expect(html).not.toHaveAttribute("data-theme");
    expect(useStore.getState().notice).toContain("disk full");
  });

  it.each(["Continue", "Skip this step"])(
    "%s moves on at once, while the pick is still saving",
    async (button) => {
      render(<Onboarding />);
      await toAppearance();
      let finishSave: () => void = () => {};
      updateSettings.mockImplementationOnce(
        () =>
          new Promise<void>((resolve) => {
            finishSave = resolve;
          }),
      );
      await pick("Dark");

      await click(button);

      expect(await screen.findByRole("heading", { name: NOTIFICATIONS_HEADING })).toBeInTheDocument();
      await act(async () => {
        finishSave();
      });
      // The pick outlives the step it was made in.
      expect(useStore.getState().settings?.theme).toBe("dark");
      expect(html).toHaveAttribute("data-theme", "dark");
      expect(themesWritten()).toEqual(["dark"]);
    },
  );

  it.each(["Continue", "Skip this step"])(
    "%s with nothing picked moves on and writes no theme",
    async (button) => {
      render(<Onboarding />);
      await toAppearance();

      await click(button);

      expect(await screen.findByRole("heading", { name: NOTIFICATIONS_HEADING })).toBeInTheDocument();
      noDialog();
      // Still just the name step's own skip: leaving here adds no write of its own.
      expect(updateSettings).toHaveBeenCalledTimes(1);
      expect(themesWritten()).toEqual([]);
      expect(html).not.toHaveAttribute("data-theme");
      expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
    },
  );

  it("states what leaving costs on Skip setup, and Cancel returns with the pick intact", async () => {
    render(<Onboarding />);
    await toAppearance();
    await pick("Dark");

    await openSkipSetup();

    expect(within(skipDialog()).getByText(MSG_NO_MODEL)).toBeInTheDocument();
    expect(completedSaves()).toHaveLength(0);
    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));

    noDialog();
    expect(screen.getByRole("heading", { name: APPEARANCE_HEADING })).toBeInTheDocument();
    expect(radio("Dark")).toBeChecked();
    expect(html).toHaveAttribute("data-theme", "dark");
  });

  it("keeps the pick when Skip setup is confirmed", async () => {
    // A backend that remembers what it is sent, as the real one does: the static
    // default would hand `completeOnboarding` a settings object that never saw the pick.
    let saved: Record<string, unknown> = { onboardingCompleted: false };
    updateSettings.mockImplementation(async (settings) => {
      saved = settings;
    });
    getSettings.mockImplementation(async () => saved);
    render(<Onboarding />);
    await toAppearance();
    await pick("Dark");

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(completedSaves()).toHaveLength(1));
    expect(completedSaves()[0][0]).toEqual(expect.objectContaining({ theme: "dark" }));
    expect(useStore.getState().settings?.theme).toBe("dark");
    expect(html).toHaveAttribute("data-theme", "dark");
  });

  it("leaves the theme on System when Skip setup is confirmed with nothing picked", async () => {
    render(<Onboarding />);
    await toAppearance();

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(completedSaves()).toHaveLength(1));
    expect(themesWritten()).toEqual([]);
    expect(html).not.toHaveAttribute("data-theme");
  });

  it("works from the keyboard alone: one Tab stop on the checked option, and the arrows pick", async () => {
    render(<Onboarding />);
    await toAppearance();
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: APPEARANCE_HEADING })).toHaveFocus(),
    );

    await userEvent.tab();
    expect(radio("Use system default")).toHaveFocus();

    await userEvent.keyboard("{ArrowRight}");
    expect(radio("Light")).toBeChecked();
    expect(radio("Light")).toHaveFocus();
    expect(html).toHaveAttribute("data-theme", "light");

    await userEvent.keyboard("{ArrowRight}");
    expect(radio("Dark")).toBeChecked();
    expect(html).toHaveAttribute("data-theme", "dark");
    expect(themesWritten()).toEqual(["light", "dark"]);

    // The group is a single Tab stop, so the next one is the button below it.
    await userEvent.tab();
    expect(screen.getByRole("button", { name: "Continue" })).toHaveFocus();
  });
});

describe("Onboarding: the notifications step", () => {
  /** The `desktopNotifications` of every settings write that carries one, in the
   * order they were sent. The seeded settings have none, so a write only shows up
   * here if this step (or the switch behind it) put the flag in it. */
  const notificationsWritten = () =>
    updateSettings.mock.calls.flatMap(([s]) =>
      s?.desktopNotifications === undefined ? [] : [s.desktopNotifications],
    );

  it("comes right after the appearance step and before the hardware scan", async () => {
    render(<Onboarding />);
    await toAppearance();

    await click("Continue");

    expect(await screen.findByRole("heading", { name: NOTIFICATIONS_HEADING })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: SCAN_HEADING })).not.toBeInTheDocument();
    expect(screen.getByText(/OpenPal can notify you when a model loads/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Turn on notifications" })).toBeInTheDocument();
  });

  it("takes focus to the heading on arrival, and arriving saves and asks nothing", async () => {
    render(<Onboarding />);
    await toNotifications();

    await waitFor(() =>
      expect(screen.getByRole("heading", { name: NOTIFICATIONS_HEADING })).toHaveFocus(),
    );
    expect(notificationsWritten()).toEqual([]);
    expect(requestNotificationPermission).not.toHaveBeenCalled();
    // Optional like the steps around it: both ways out are live from the first frame.
    expect(screen.getByRole("button", { name: "Skip this step" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip setup" })).toBeEnabled();
  });

  it("Skip this step moves on to the scan and leaves notifications off", async () => {
    render(<Onboarding />);
    await toNotifications();

    await skipStep();

    expect(await screen.findByRole("heading", { name: SCAN_HEADING })).toBeInTheDocument();
    noDialog();
    expect(notificationsWritten()).toEqual([]);
    expect(requestNotificationPermission).not.toHaveBeenCalled();
    expect(useStore.getState().settings?.desktopNotifications).not.toBe(true);
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it("Turn on notifications saves the choice, asks the OS once, and moves on", async () => {
    render(<Onboarding />);
    await toNotifications();

    await click("Turn on notifications");

    expect(await screen.findByRole("heading", { name: SCAN_HEADING })).toBeInTheDocument();
    await waitFor(() => expect(notificationsWritten()).toEqual([true]));
    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
    await waitFor(() => expect(requestNotificationPermission).toHaveBeenCalledTimes(1));
    // Asking is the only OS call: the read-only one has nothing to add.
    expect(notificationPermission).not.toHaveBeenCalled();
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it("moves on at once, without waiting for the save (and so the OS) to answer", async () => {
    render(<Onboarding />);
    await toNotifications();
    // The write never settles, so the permission request behind it never starts
    // either: nothing the OS or the disk is slow to answer may strand the user here.
    updateSettings.mockImplementationOnce(() => new Promise<void>(() => {}));

    await click("Turn on notifications");

    expect(await screen.findByRole("heading", { name: SCAN_HEADING })).toBeInTheDocument();
    // The store already has the choice, so what follows (Settings) shows it.
    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
  });

  it("still moves on, with the choice kept and no notice, when the OS says it is blocked", async () => {
    requestNotificationPermission.mockResolvedValue("denied");
    render(<Onboarding />);
    await toNotifications();

    await click("Turn on notifications");

    expect(await screen.findByRole("heading", { name: SCAN_HEADING })).toBeInTheDocument();
    await waitFor(() => expect(useStore.getState().notificationPermission).toBe("denied"));
    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
    expect(useStore.getState().notice).toBeNull();
  });

  it("Skip setup asks first, and confirming leaves notifications off", async () => {
    render(<Onboarding />);
    await toNotifications();

    await openSkipSetup();
    expect(within(skipDialog()).getByText(MSG_NO_MODEL)).toBeInTheDocument();
    expect(completedSaves()).toHaveLength(0);
    await confirmSkip();

    await waitFor(() => expect(completedSaves()).toHaveLength(1));
    expect(notificationsWritten()).toEqual([]);
    expect(requestNotificationPermission).not.toHaveBeenCalled();
  });

  it("is worded for a phone on Android, which has notifications but no desktop", async () => {
    useStore.setState({ appInfo: { isAndroid: true, nativeNotifications: true } as AppInfo });
    render(<Onboarding />);
    await toNotifications();

    expect(screen.getByText(/when a download finishes or an answer is ready, while you're using another app/)).toBeInTheDocument();
    expect(screen.queryByText(/in another window/)).not.toBeInTheDocument();
  });

  it("is left out in a build with no OS notifications, where appearance goes straight to the scan", async () => {
    useStore.setState({ appInfo: { isAndroid: false, nativeNotifications: false } as AppInfo });
    render(<Onboarding />);
    await toAppearance();

    await skipStep();

    expect(await screen.findByRole("heading", { name: SCAN_HEADING })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: NOTIFICATIONS_HEADING })).not.toBeInTheDocument();
    expect(notificationsWritten()).toEqual([]);
  });

  it("is all in French when the app is", async () => {
    await i18n.changeLanguage("fr");
    try {
      render(<Onboarding />);
      await userEvent.click(screen.getByRole("button", { name: "Configurons tout ça" }));
      await screen.findByRole("heading", { name: "Comment devons-nous vous appeler ?" });
      // The name step's skip, then the appearance step's.
      await userEvent.click(screen.getByRole("button", { name: "Passer cette étape" }));
      await screen.findByRole("heading", { name: "Quelle apparence souhaitez-vous pour OpenPal ?" });
      await userEvent.click(screen.getByRole("button", { name: "Passer cette étape" }));

      expect(
        await screen.findByRole("heading", { name: "Souhaitez-vous être prévenu ?" }),
      ).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Activer les notifications" })).toBeInTheDocument();
    } finally {
      await i18n.changeLanguage("en");
    }
  });
});

describe("Onboarding: the final screen follows whether a model exists", () => {
  it("with no model it is almost there, says to pick one in Models, and offers Get started", async () => {
    render(<Onboarding />);
    await toReady();

    expect(screen.getByRole("heading", { name: "Almost there." })).toBeInTheDocument();
    expect(screen.getByText(FINAL_NO_MODEL)).toBeInTheDocument();
    expect(screen.queryByText(/is awake and listening/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Say hello" })).not.toBeInTheDocument();
    // It has no skip of its own: its one button finishes.
    expect(screen.queryByRole("button", { name: /skip/i })).not.toBeInTheDocument();

    await click("Get started");

    await waitFor(() => expect(useStore.getState().settings?.onboardingCompleted).toBe(true));
  });

  it("with a model running it is all set, says it is awake, and offers Say hello", async () => {
    render(<Onboarding />);
    await toReady();

    act(() => useStore.setState({ engine: RUNNING }));

    expect(await screen.findByRole("heading", { name: "All set." })).toBeInTheDocument();
    expect(
      screen.getByText(
        "Qwen 3 4B is awake and listening. It runs entirely on your Mac, even with the internet off.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Say hello" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Get started" })).not.toBeInTheDocument();
  });

  it("names the model the engine is actually running, not the recommended one", async () => {
    useStore.setState({ models: [MODEL, ALT] });
    render(<Onboarding />);
    await toReady();

    act(() =>
      useStore.setState({
        engine: {
          type: "running",
          data: { modelId: "alt-model", port: 1, diskSlow: false, autoUnloadAt: null },
        },
      }),
    );

    expect(
      await screen.findByText(
        "Alt Model 1B is awake and listening. It runs entirely on your Mac, even with the internet off.",
      ),
    ).toBeInTheDocument();
  });

  it("while the model is still loading it says so, instead of claiming it is awake", async () => {
    render(<Onboarding />);
    await toReady();

    act(() => useStore.setState({ engine: STARTING }));

    expect(
      await screen.findByText("Loading Qwen 3 4B into memory. Just a moment."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/is awake and listening/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Say hello" })).toBeInTheDocument();
  });

  it("a failed engine reads as no model", async () => {
    render(<Onboarding />);
    await toReady();

    act(() => useStore.setState({ engine: FAILED_ENGINE }));

    expect(screen.getByText(FINAL_NO_MODEL)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Get started" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Say hello" })).not.toBeInTheDocument();
  });

  it("the line under the headline is a live region, since it follows the engine", async () => {
    render(<Onboarding />);
    await toReady();

    expect(screen.getByRole("status")).toHaveTextContent(FINAL_NO_MODEL);
    act(() => useStore.setState({ engine: STARTING }));
    expect(screen.getByRole("status")).toHaveTextContent(
      "Loading Qwen 3 4B into memory. Just a moment.",
    );
  });

  it("after a download was stopped it says so, and that it can be resumed", async () => {
    render(<Onboarding />);
    await toDownload();
    await skipStep(); // stops the download in flight
    await screen.findByRole("heading", { name: "Add a PIN?" });

    await skipStep();

    expect(await screen.findByText(FINAL_DOWNLOAD_STOPPED)).toBeInTheDocument();
    expect(screen.queryByText(FINAL_NO_MODEL)).not.toBeInTheDocument();
  });

  it("without a stopped download it gives the plain no-model text", async () => {
    // The model setup skipped from the scan screen: nothing was ever downloading.
    render(<Onboarding />);
    await toReady();

    expect(screen.queryByText(FINAL_DOWNLOAD_STOPPED)).not.toBeInTheDocument();
    expect(screen.getByText(FINAL_NO_MODEL)).toBeInTheDocument();
  });
});

describe("Onboarding: Skip setup asks before leaving", () => {
  it("states what leaving costs, and Cancel stays put", async () => {
    render(<Onboarding />);

    await openSkipSetup();

    expect(within(skipDialog()).getByText(MSG_NO_MODEL)).toBeInTheDocument();
    // The consequence is what a screen reader hears with the title.
    expect(skipDialog()).toHaveAccessibleDescription(MSG_NO_MODEL);
    expect(updateSettings).not.toHaveBeenCalled();

    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));

    noDialog();
    expect(screen.getByRole("button", { name: "Let's set things up" })).toBeInTheDocument();
    expect(updateSettings).not.toHaveBeenCalled();
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
  });

  it("the confirm button is the plain accent button, not the red destructive one", async () => {
    render(<Onboarding />);
    await openSkipSetup();

    const confirm = within(skipDialog()).getByRole("button", { name: "Skip setup" });
    expect(confirm).toHaveClass("btn");
    expect(confirm).not.toHaveClass("btn-danger-quiet");
  });

  it("Escape dismisses it without skipping and hands focus back to Skip setup", async () => {
    render(<Onboarding />);
    const opener = screen.getByRole("button", { name: "Skip setup" });

    await userEvent.click(opener);
    expect(skipDialog()).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");

    noDialog();
    expect(opener).toHaveFocus();
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it("a backdrop click dismisses it; a click inside it does not", async () => {
    render(<Onboarding />);
    await openSkipSetup();

    await userEvent.click(within(skipDialog()).getByText(MSG_NO_MODEL));
    expect(skipDialog()).toBeInTheDocument();

    await userEvent.click(skipDialog().parentElement!);
    noDialog();
    expect(updateSettings).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Let's set things up" })).toBeInTheDocument();
  });

  it("confirming completes onboarding without touching any download", async () => {
    render(<Onboarding />);

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(completedSaves()).toHaveLength(1));
    expect(useStore.getState().settings?.onboardingCompleted).toBe(true);
    expect(cancelDownload).not.toHaveBeenCalled();
  });

  it("warns that a download in flight will stop, and Cancel leaves it running", async () => {
    render(<Onboarding />);
    await toDownload();

    await openSkipSetup();

    expect(within(skipDialog()).getByText(MSG_DOWNLOADING)).toBeInTheDocument();

    await userEvent.click(within(skipDialog()).getByRole("button", { name: "Cancel" }));

    expect(cancelDownload).not.toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "Bringing Qwen 3 4B home…" })).toBeInTheDocument();
  });

  it("confirming mid-download completes onboarding, and only then drops the download", async () => {
    render(<Onboarding />);
    await toDownload();

    await openSkipSetup();
    // Nothing is cancelled or completed until the skip is confirmed.
    expect(cancelDownload).not.toHaveBeenCalled();
    expect(completedSaves()).toHaveLength(0);
    await confirmSkip();

    await waitFor(() => expect(cancelDownload).toHaveBeenCalledWith("qwen3-4b"));
    expect(useStore.getState().settings?.onboardingCompleted).toBe(true);
    // Saved first: cancelling first would race the store's post-download
    // settings re-read against the save (and a failed save would lose it).
    expect(cancelDownload.mock.invocationCallOrder[0]).toBeGreaterThan(
      updateSettings.mock.invocationCallOrder[
        updateSettings.mock.calls.findIndex(([s]) => s?.onboardingCompleted === true)
      ],
    );
  });

  it("confirming with nothing in flight cancels no model", async () => {
    // A failed download: the scene lands on the error branch with nothing to cancel.
    downloadModel.mockRejectedValueOnce(new Error("download failed"));
    render(<Onboarding />);
    await toRecommend();
    await click(/^Download/);
    await screen.findByRole("heading", { name: "Hmm, that didn't finish." });

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(useStore.getState().settings?.onboardingCompleted).toBe(true));
    expect(cancelDownload).not.toHaveBeenCalled();
  });

  it.each([
    ["running", RUNNING],
    ["loading", STARTING],
  ])("says the model is installed while it is %s", async (_state, engine) => {
    render(<Onboarding />);
    await toSecurity();
    act(() => useStore.setState({ engine }));

    await openSkipSetup();

    expect(within(skipDialog()).getByText(MSG_MODEL_READY)).toBeInTheDocument();
  });

  it("a download in flight outranks a running engine in the wording", async () => {
    useStore.setState({
      engine: RUNNING,
      downloads: { "qwen3-4b": { receivedBytes: 1, totalBytes: 100, bytesPerSec: 0, etaSeconds: 0 } },
    });
    render(<Onboarding />);

    await openSkipSetup();

    expect(within(skipDialog()).getByText(MSG_DOWNLOADING)).toBeInTheDocument();
  });

  it("confirming mid-download drops the model the user picked, not the recommended one", async () => {
    useStore.setState({
      models: [MODEL, ALT],
      report: {
        hardware: HARDWARE,
        recommendation: {
          primaryModelId: "qwen3-4b",
          headline: "Qwen 3 4B is the best fit for your Mac.",
          reasons: ["Fits easily in your memory."],
          ranked: [
            { modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." },
            { modelId: "alt-model", fit: "good", why: "Fits fine." },
          ],
        },
      } as SystemReport,
    });
    render(<Onboarding />);
    await toRecommend();
    await click("Pick a different one");
    await click(/Alt Model 1B/);
    await click(/^Download/);
    await screen.findByRole("heading", { name: "Bringing Alt Model 1B home…" });

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => expect(cancelDownload).toHaveBeenCalledWith("alt-model"));
    expect(cancelDownload).toHaveBeenCalledTimes(1);
  });

  it("stays up while the skip is saving, even if the scene moves on underneath it", async () => {
    let finishSave: () => void = () => {};
    render(<Onboarding />);
    await toDownload();
    updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finishSave = resolve;
        }),
    );
    await openSkipSetup();
    await confirmSkip();

    // The download finishes and the engine comes up while the save is in flight.
    act(() => useStore.setState({ models: [INSTALLED], downloads: {}, engine: RUNNING }));
    await screen.findByRole("heading", { name: "Add a PIN?" });

    // The skip is already underway, so the dialog doesn't vanish and hand back a
    // live PIN offer that the save is about to leave anyway.
    expect(skipDialog()).toBeInTheDocument();
    expect(within(skipDialog()).getByRole("button", { name: "Skip setup" })).toBeDisabled();

    finishSave();
    await waitFor(() => expect(useStore.getState().settings?.onboardingCompleted).toBe(true));
    // The download had finished, so there is nothing to stop — but a stale cancel is harmless.
    expect(completedSaves()).toHaveLength(1);
  });

  it("locks while the skip is saving: buttons, Escape and the backdrop are all inert", async () => {
    let finishSave: () => void = () => {};
    updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finishSave = resolve;
        }),
    );
    render(<Onboarding />);
    await openSkipSetup();
    await confirmSkip();

    const dialog = skipDialog();
    expect(within(dialog).getByRole("button", { name: "Skip setup" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeDisabled();
    // Escape is handled on the dialog itself, so fire it there (focus may have
    // left the now-disabled button); the backdrop is the dialog's parent.
    fireEvent.keyDown(dialog, { key: "Escape" });
    await userEvent.click(dialog.parentElement!);
    expect(skipDialog()).toBeInTheDocument();
    expect(updateSettings).toHaveBeenCalledTimes(1);

    finishSave();
    await waitFor(() => expect(useStore.getState().settings?.onboardingCompleted).toBe(true));
  });

  it("hands the scene back when saving fails, and a second try is not stuck busy", async () => {
    // `completeOnboarding` swallows the failure into a notice and leaves
    // onboarding open — the dialog must close, and not stay locked for next time.
    updateSettings.mockRejectedValueOnce(new Error("disk full"));
    render(<Onboarding />);

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => noDialog());
    expect(screen.getByRole("button", { name: "Let's set things up" })).toBeInTheDocument();
    expect(useStore.getState().settings?.onboardingCompleted).not.toBe(true);
    expect(useStore.getState().notice).toContain("disk full");

    // Second attempt: the Once is spent, so the save now succeeds.
    await openSkipSetup();
    const dialog = skipDialog();
    expect(within(dialog).getByRole("button", { name: "Skip setup" })).toBeEnabled();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeEnabled();
    await confirmSkip();

    await waitFor(() => expect(useStore.getState().settings?.onboardingCompleted).toBe(true));
    expect(updateSettings).toHaveBeenCalledTimes(2);
  });

  it("keeps the download running when saving the skip fails", async () => {
    render(<Onboarding />);
    await toDownload();
    // Armed only now, so it is the skip's own save that fails (the name step
    // earlier in the flow saved fine).
    updateSettings.mockRejectedValueOnce(new Error("disk full"));

    await openSkipSetup();
    await confirmSkip();

    await waitFor(() => noDialog());
    expect(cancelDownload).not.toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "Bringing Qwen 3 4B home…" })).toBeInTheDocument();
  });

  it("closes when the scene moves on underneath it, instead of going stale", async () => {
    render(<Onboarding />);
    await toDownload();
    await openSkipSetup();
    expect(within(skipDialog()).getByText(MSG_DOWNLOADING)).toBeInTheDocument();

    // The download finishes and the engine comes up: Onboarding moves to the
    // PIN offer on its own, so "your download will stop" no longer applies.
    act(() => useStore.setState({ models: [INSTALLED], downloads: {}, engine: RUNNING }));

    const heading = await screen.findByRole("heading", { name: "Add a PIN?" });
    noDialog();
    // Focus follows the new scene, rather than sitting behind a modal that is gone.
    expect(heading).toHaveFocus();
    expect(completedSaves()).toHaveLength(0);
  });
});

describe("Onboarding: picking a different model", () => {
  /** The recommended model plus one alternative, as a recommendation lists them. */
  function seedTwoModels() {
    useStore.setState({
      models: [MODEL, ALT],
      report: {
        hardware: HARDWARE,
        recommendation: {
          primaryModelId: "qwen3-4b",
          headline: "Qwen 3 4B is the best fit for your Mac.",
          reasons: ["Fits easily in your memory."],
          ranked: [
            { modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." },
            { modelId: "alt-model", fit: "good", why: "Fits fine." },
          ],
        },
      } as SystemReport,
    });
  }

  const rows = () =>
    Array.from(document.querySelectorAll<HTMLElement>(".ob-alt-row")).map((r) => r.textContent);

  it("swaps the card for a list, recommended model first and tagged", async () => {
    seedTwoModels();
    render(<Onboarding />);
    await toRecommend();

    await click("Pick a different one");

    // The big card is gone — nothing is pushed down by the list any more.
    expect(screen.queryByRole("heading", { name: "Qwen 3 4B" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Download/ })).not.toBeInTheDocument();
    const [first, second] = screen.getAllByRole("button", { name: /GB/ });
    expect(first).toHaveTextContent("Qwen 3 4B");
    expect(first).toHaveTextContent("Recommended");
    expect(second).toHaveTextContent("Alt Model 1B");
    expect(second).not.toHaveTextContent("Recommended");
    expect(rows()).toHaveLength(2);
  });

  it("moves keyboard focus onto the list when it opens", async () => {
    seedTwoModels();
    render(<Onboarding />);
    await toRecommend();

    await click("Pick a different one");

    expect(screen.getByRole("button", { name: /Qwen 3 4B/ })).toHaveFocus();
  });

  it("'Hide other options' brings the card back and hands focus to its toggle", async () => {
    seedTwoModels();
    render(<Onboarding />);
    await toRecommend();
    await click("Pick a different one");

    await click("Hide other options");

    expect(screen.getByRole("heading", { name: "Qwen 3 4B" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pick a different one" })).toHaveFocus();
    expect(rows()).toHaveLength(0);
  });

  it("keeps the recommendation first and tagged after another model was picked", async () => {
    seedTwoModels();
    render(<Onboarding />);
    await toRecommend();
    await click("Pick a different one");
    await click(/Alt Model 1B/);
    await screen.findByRole("heading", { name: "Alt Model 1B" });

    await click("Pick a different one");

    const [first, second] = screen.getAllByRole("button", { name: /GB/ });
    expect(first).toHaveTextContent("Qwen 3 4B");
    expect(first).toHaveTextContent("Recommended");
    expect(first).not.toHaveAttribute("aria-current");
    expect(second).toHaveTextContent("Alt Model 1B");
    expect(second).toHaveAttribute("aria-current", "true");

    // And the recommendation is one click back.
    await click(/Qwen 3 4B/);
    expect(await screen.findByRole("heading", { name: "Qwen 3 4B" })).toBeInTheDocument();
  });
});
