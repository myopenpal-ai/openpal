import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useStore } from "./store";
import type { Conversation, Settings } from "../lib/types";

// The store's "Desktop notifications" switch (`setDesktopNotifications`), the
// OS permission behind its "blocked" line (`notificationPermission` only reads
// it, `requestNotificationPermission` asks — on macOS and Android: shows the
// system prompt — and only the switch turning on does that), and what a click
// on a notification opens (`followNotificationClick`). A file of its own, with
// an api mock that offers only the calls these touch, so a stray call to
// anything else throws instead of being quietly answered; store.test.ts holds
// the rest of the store's behaviour.

const {
  updateSettings,
  notificationPermission,
  requestNotificationPermission,
  takeNotificationClick,
  getConversation,
} = vi.hoisted(() => ({
  updateSettings: vi.fn(),
  notificationPermission: vi.fn(),
  requestNotificationPermission: vi.fn(),
  takeNotificationClick: vi.fn(),
  getConversation: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  api: {
    updateSettings,
    notificationPermission,
    requestNotificationPermission,
    takeNotificationClick,
    getConversation,
  },
}));

const pristine = useStore.getState();

/** A saved settings row; `desktopNotifications: undefined` stands for one saved
 * before the field existed. */
const settingsWith = (desktopNotifications?: boolean) =>
  ({ onboardingCompleted: true, language: "en", userName: "Ada", desktopNotifications }) as Settings;

/** An `updateSettings` call the test settles by hand. */
function heldWrite() {
  let settle: () => void = () => {};
  let fail: (reason: unknown) => void = () => {};
  updateSettings.mockImplementationOnce(
    () =>
      new Promise<void>((resolve, reject) => {
        settle = resolve;
        fail = reject;
      }),
  );
  return { settle: () => settle(), fail: (reason: unknown) => fail(reason) };
}

beforeEach(() => {
  updateSettings.mockReset().mockResolvedValue(undefined);
  notificationPermission.mockReset().mockResolvedValue("granted");
  requestNotificationPermission.mockReset().mockResolvedValue("granted");
  takeNotificationClick.mockReset().mockResolvedValue(null);
  getConversation.mockReset().mockImplementation(async (id: string) => ({ id }) as Conversation);
  useStore.setState(pristine, true);
});

afterEach(() => {
  useStore.setState(pristine, true);
});

describe("notificationPermission", () => {
  it("starts as granted, so nothing says 'blocked' before the OS has been asked", () => {
    expect(useStore.getState().notificationPermission).toBe("granted");
  });

  it("records what the OS answers", async () => {
    notificationPermission.mockResolvedValueOnce("denied");
    await useStore.getState().refreshNotificationPermission();
    expect(useStore.getState().notificationPermission).toBe("denied");

    notificationPermission.mockResolvedValueOnce("granted");
    await useStore.getState().refreshNotificationPermission();
    expect(useStore.getState().notificationPermission).toBe("granted");
  });

  it("keeps the last answer, and throws nothing, when the read fails", async () => {
    useStore.setState({ notificationPermission: "denied" });
    notificationPermission.mockRejectedValueOnce(new Error("no bridge"));

    await expect(useStore.getState().refreshNotificationPermission()).resolves.toBeUndefined();

    // Not knowing is not the same as being allowed: nothing flips.
    expect(useStore.getState().notificationPermission).toBe("denied");
    expect(useStore.getState().notice).toBeNull();
  });
});

describe("requestNotificationPermission", () => {
  it("records what the OS answers after being asked, without touching the read-only call", async () => {
    requestNotificationPermission.mockResolvedValueOnce("denied");
    await useStore.getState().requestNotificationPermission();
    expect(useStore.getState().notificationPermission).toBe("denied");
    expect(requestNotificationPermission).toHaveBeenCalledTimes(1);
    expect(notificationPermission).not.toHaveBeenCalled();
  });

  it("keeps the last answer, and throws nothing, when the ask fails", async () => {
    useStore.setState({ notificationPermission: "denied" });
    requestNotificationPermission.mockRejectedValueOnce(new Error("no bridge"));

    await expect(useStore.getState().requestNotificationPermission()).resolves.toBeUndefined();

    expect(useStore.getState().notificationPermission).toBe("denied");
    expect(useStore.getState().notice).toBeNull();
  });
});

describe("setDesktopNotifications", () => {
  it("is optimistic: the store changes while the write is still pending", async () => {
    useStore.setState({ settings: settingsWith(false) });
    const write = heldWrite();

    const pick = useStore.getState().setDesktopNotifications(true);

    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
    expect(updateSettings).toHaveBeenCalledTimes(1);
    // The OS is only asked once the choice is safely saved.
    expect(requestNotificationPermission).not.toHaveBeenCalled();

    write.settle();
    await pick;
    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
    expect(requestNotificationPermission).toHaveBeenCalledTimes(1);
  });

  it("persists the whole settings object with the flag flipped", async () => {
    const stored = settingsWith(false);
    useStore.setState({ settings: stored });

    await useStore.getState().setDesktopNotifications(true);

    expect(updateSettings).toHaveBeenCalledTimes(1);
    expect(updateSettings).toHaveBeenCalledWith({ ...stored, desktopNotifications: true });
  });

  it("asks the OS only when turned on, and records a refusal without a notice", async () => {
    useStore.setState({ settings: settingsWith(true) });
    await useStore.getState().setDesktopNotifications(false);
    expect(useStore.getState().settings?.desktopNotifications).toBe(false);
    expect(requestNotificationPermission).not.toHaveBeenCalled();

    requestNotificationPermission.mockResolvedValueOnce("denied");
    await useStore.getState().setDesktopNotifications(true);

    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
    expect(requestNotificationPermission).toHaveBeenCalledTimes(1);
    // Turning on asks; it never merely reads.
    expect(notificationPermission).not.toHaveBeenCalled();
    expect(useStore.getState().notificationPermission).toBe("denied");
    // A refusal by the OS is not an error to show.
    expect(useStore.getState().notice).toBeNull();
  });

  it("doesn't raise the OS prompt for a switch the user has already turned off again", async () => {
    useStore.setState({ settings: settingsWith(false) });
    const write = heldWrite(); // the "on" write is slow...
    const on = useStore.getState().setDesktopNotifications(true);
    // ...and the user changes their mind before it lands.
    const off = useStore.getState().setDesktopNotifications(false);

    write.settle();
    await Promise.all([on, off]);

    expect(useStore.getState().settings?.desktopNotifications).toBe(false);
    expect(requestNotificationPermission).not.toHaveBeenCalled();
  });

  it.each([
    ["off over a saved off", settingsWith(false), false],
    ["off over a row saved before the setting existed", settingsWith(undefined), false],
    ["on over a saved on", settingsWith(true), true],
  ])("writes nothing for %s", async (_what, stored, pick) => {
    useStore.setState({ settings: stored });

    await useStore.getState().setDesktopNotifications(pick);

    expect(updateSettings).not.toHaveBeenCalled();
    expect(requestNotificationPermission).not.toHaveBeenCalled();
    expect(useStore.getState().settings).toBe(stored);
  });

  it("turns on over a row saved before the setting existed", async () => {
    useStore.setState({ settings: settingsWith(undefined) });

    await useStore.getState().setDesktopNotifications(true);

    expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ desktopNotifications: true, userName: "Ada" }),
    );
  });

  it("does nothing before settings have loaded", async () => {
    await useStore.getState().setDesktopNotifications(true);

    expect(updateSettings).not.toHaveBeenCalled();
    expect(requestNotificationPermission).not.toHaveBeenCalled();
    expect(useStore.getState().settings).toBeNull();
  });

  it("puts the old value back, sets a notice and never asks the OS, when the write fails", async () => {
    useStore.setState({ settings: settingsWith(false) });
    updateSettings.mockRejectedValueOnce(new Error("disk full"));

    const pick = useStore.getState().setDesktopNotifications(true);
    expect(useStore.getState().settings?.desktopNotifications).toBe(true); // at once
    await pick;

    expect(useStore.getState().settings?.desktopNotifications).toBe(false);
    expect(useStore.getState().notice).toBe("Error: disk full");
    expect(requestNotificationPermission).not.toHaveBeenCalled();
  });

  it("puts a failed turn-off back to on, not to off", async () => {
    useStore.setState({ settings: settingsWith(true) });
    updateSettings.mockRejectedValueOnce(new Error("disk full"));

    await useStore.getState().setDesktopNotifications(false);

    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
  });

  it("does not roll an older failed pick back over a newer one", async () => {
    useStore.setState({ settings: settingsWith(false) });
    const first = heldWrite();
    const second = heldWrite();

    const olderPick = useStore.getState().setDesktopNotifications(true);
    const newerPick = useStore.getState().setDesktopNotifications(false);
    first.fail(new Error("disk full"));
    await olderPick;

    // The newer pick is the one on screen, and its own write is still out.
    expect(useStore.getState().settings?.desktopNotifications).toBe(false);

    second.settle();
    await newerPick;
    expect(useStore.getState().settings?.desktopNotifications).toBe(false);
  });

  it("ends on the value that is on disk when both of two overlapping picks fail", async () => {
    // On, then off again, both writes rejected. The newer pick's "before" is
    // the older pick's optimistic on, which was never saved: restoring that
    // would leave the switch showing on over a disk that says off.
    useStore.setState({ settings: settingsWith(false) });
    const first = heldWrite();
    const second = heldWrite();

    const olderPick = useStore.getState().setDesktopNotifications(true);
    const newerPick = useStore.getState().setDesktopNotifications(false);
    first.fail(new Error("disk full"));
    await olderPick;
    second.fail(new Error("disk full"));
    await newerPick;

    expect(useStore.getState().settings?.desktopNotifications).toBe(false);
  });

  it("ends on the value that did get saved when the older pick lands and the newer one fails", async () => {
    useStore.setState({ settings: settingsWith(false) });
    const first = heldWrite();
    const second = heldWrite();

    const olderPick = useStore.getState().setDesktopNotifications(true);
    const newerPick = useStore.getState().setDesktopNotifications(false);
    first.settle();
    await olderPick;
    second.fail(new Error("disk full"));
    await newerPick;

    // The on was saved; the off was not. The switch shows what is on disk.
    expect(useStore.getState().settings?.desktopNotifications).toBe(true);
  });

  it("gives a later saveSettings a base that already carries the flag", async () => {
    useStore.setState({ settings: settingsWith(false) });
    const write = heldWrite();
    const pick = useStore.getState().setDesktopNotifications(true);

    // The pick's write is still in flight when Settings closes and saves its
    // drafts: that save must not write the old flag back.
    await useStore.getState().saveSettings({ userName: "Grace" });

    expect(updateSettings).toHaveBeenLastCalledWith({
      ...settingsWith(true),
      userName: "Grace",
    });
    write.settle();
    await pick;
    expect(useStore.getState().settings).toEqual({ ...settingsWith(true), userName: "Grace" });
  });
});

describe("followNotificationClick", () => {
  it("leaves the click with Rust until settings have loaded, so a locked app loses nothing", async () => {
    takeNotificationClick.mockResolvedValue({ conversationId: "c1" });
    await useStore.getState().followNotificationClick();
    expect(takeNotificationClick).not.toHaveBeenCalled();
    expect(getConversation).not.toHaveBeenCalled();
  });

  it("opens the clicked answer's conversation in the chat, with nothing laid over it", async () => {
    useStore.setState({
      settings: settingsWith(true),
      view: "translate",
      moreDialogPage: "library",
      conversationSearchOpen: true,
    });
    takeNotificationClick.mockResolvedValue({ conversationId: "c1" });

    await useStore.getState().followNotificationClick();

    expect(takeNotificationClick).toHaveBeenCalledTimes(1);
    expect(getConversation).toHaveBeenCalledWith("c1");
    const s = useStore.getState();
    expect(s.active?.id).toBe("c1");
    expect(s.view).toBe("chat");
    expect(s.moreDialogPage).toBeNull();
    expect(s.conversationSearchOpen).toBe(false);
  });

  it("changes nothing for a click that names no conversation, or for no click at all", async () => {
    useStore.setState({ settings: settingsWith(true), moreDialogPage: "library" });

    takeNotificationClick.mockResolvedValueOnce({ conversationId: null });
    await useStore.getState().followNotificationClick();
    takeNotificationClick.mockResolvedValueOnce(null);
    await useStore.getState().followNotificationClick();

    // Rust has already brought the window forward; there is nothing to open.
    expect(getConversation).not.toHaveBeenCalled();
    expect(useStore.getState().moreDialogPage).toBe("library");
  });

  it("never throws when the click cannot be read", async () => {
    useStore.setState({ settings: settingsWith(true) });
    takeNotificationClick.mockRejectedValueOnce(new Error("no bridge"));
    await expect(useStore.getState().followNotificationClick()).resolves.toBeUndefined();
    expect(getConversation).not.toHaveBeenCalled();
  });
});
