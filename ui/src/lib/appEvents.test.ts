import { afterAll, beforeEach, describe, expect, it } from "vitest";
import i18n from "../i18n";
import {
  alertCopy,
  type AppEvent,
  decideDelivery,
  type DeliveryContext,
  hasNativeNotifications,
  isConversationOpenInChat,
} from "./appEvents";
import { engineModelName } from "./format";
import type { ModelInfo } from "./types";

// Each test starts from a known language, whatever an earlier one left behind.
beforeEach(async () => {
  await i18n.changeLanguage("en");
});
afterAll(async () => {
  await i18n.changeLanguage("en");
});

const answer = (conversationId = "convo-a"): AppEvent => ({
  kind: "answerReady",
  key: `answer:${conversationId}`,
  conversationId,
});
const model = (kind: "modelLoaded" | "modelUnloaded" | "downloadFinished", modelName: string | null) =>
  ({ kind, key: `${kind}:1`, modelName }) satisfies AppEvent;

/** A window in the background, switch on, chat open on `convo-a`: the context
 * where everything is delivered; tests flip one field at a time. */
function ctx(overrides: Partial<DeliveryContext> = {}): DeliveryContext {
  return {
    view: "chat",
    active: { id: "convo-a" },
    moreDialogPage: null,
    conversationSearchOpen: false,
    desktopNotifications: true,
    windowFocused: false,
    nativeNotifications: true,
    ...overrides,
  };
}

describe("alertCopy", () => {
  it("names the model in the English copy of each model event", () => {
    expect(alertCopy(model("modelLoaded", "Qwen3 4B"))).toEqual({
      title: "Model ready",
      body: "Qwen3 4B is loaded and ready to chat.",
    });
    expect(alertCopy(model("modelUnloaded", "Qwen3 4B"))).toEqual({
      title: "Model unloaded",
      body: "Qwen3 4B is no longer loaded.",
    });
    expect(alertCopy(model("downloadFinished", "Qwen3 4B"))).toEqual({
      title: "Download complete",
      body: "Qwen3 4B has finished downloading.",
    });
  });

  it("has fixed copy for an answer", () => {
    expect(alertCopy(answer())).toEqual({
      title: "Answer ready",
      body: "OpenPal has finished answering.",
    });
  });

  it("falls back to a generic name, never an id, when the model's name is unknown", () => {
    expect(alertCopy(model("modelLoaded", null)).body).toBe("Your AI is loaded and ready to chat.");
    expect(alertCopy(model("modelUnloaded", "")).body).toBe("Your AI is no longer loaded.");
    expect(alertCopy(model("downloadFinished", null)).body).toBe("Your AI has finished downloading.");
  });

  it("follows the interface language", async () => {
    await i18n.changeLanguage("fr");
    expect(alertCopy(model("modelLoaded", "Qwen3 4B"))).toEqual({
      title: "Modèle prêt",
      body: "Qwen3 4B est chargé et prêt à discuter.",
    });
    expect(alertCopy(model("modelUnloaded", null)).body).toBe("Le modèle n'est plus chargé.");
    expect(alertCopy(answer())).toEqual({
      title: "Réponse prête",
      body: "OpenPal a fini de répondre.",
    });
  });

  it("does not HTML-escape a model name (the alert renders text, not markup)", () => {
    expect(alertCopy(model("modelLoaded", "Llama 3.1 8B <Instruct> & co")).body).toBe(
      "Llama 3.1 8B <Instruct> & co is loaded and ready to chat.",
    );
  });
});

describe("isConversationOpenInChat", () => {
  const chat = { moreDialogPage: null, conversationSearchOpen: false };

  it("is true for the open conversation in the chat view", () => {
    expect(isConversationOpenInChat({ view: "chat", active: { id: "a" }, ...chat }, "a")).toBe(true);
  });

  it("is false for a different conversation", () => {
    expect(isConversationOpenInChat({ view: "chat", active: { id: "b" }, ...chat }, "a")).toBe(false);
  });

  it("is false when no conversation is open", () => {
    expect(isConversationOpenInChat({ view: "chat", active: null, ...chat }, "a")).toBe(false);
  });

  it.each(["translate", "correct", "improve"])("is false in the %s view", (view) => {
    expect(isConversationOpenInChat({ view, active: { id: "a" }, ...chat }, "a")).toBe(false);
  });

  // The More dialog and conversation search open over the chat and leave `view`
  // as "chat": with one up, the answer is covered, not being looked at.
  it.each(["general", "library", "discover", "plugins", "skills"])(
    "is false while the More dialog is open on %s",
    (moreDialogPage) => {
      expect(
        isConversationOpenInChat(
          { view: "chat", active: { id: "a" }, moreDialogPage, conversationSearchOpen: false },
          "a",
        ),
      ).toBe(false);
    },
  );

  it("is false while conversation search is open", () => {
    expect(
      isConversationOpenInChat(
        { view: "chat", active: { id: "a" }, moreDialogPage: null, conversationSearchOpen: true },
        "a",
      ),
    ).toBe(false);
  });
});

describe("decideDelivery", () => {
  describe("the in-app alert", () => {
    it("is shown for an answer in a different conversation", () => {
      expect(decideDelivery(answer("convo-a"), ctx({ active: { id: "convo-b" } })).inApp).toBe(true);
    });

    it("is shown for an answer when nothing is open", () => {
      expect(decideDelivery(answer(), ctx({ active: null })).inApp).toBe(true);
    });

    it.each(["translate", "correct", "improve"])("is shown for an answer from the %s view", (view) => {
      expect(decideDelivery(answer("convo-a"), ctx({ view })).inApp).toBe(true);
    });

    it("is shown for an answer while the More dialog or conversation search covers the chat", () => {
      expect(decideDelivery(answer("convo-a"), ctx({ moreDialogPage: "library" })).inApp).toBe(true);
      expect(decideDelivery(answer("convo-a"), ctx({ conversationSearchOpen: true })).inApp).toBe(true);
    });

    it("is skipped for an answer in the conversation open in the chat view", () => {
      expect(decideDelivery(answer("convo-a"), ctx()).inApp).toBe(false);
    });

    it("is skipped for that answer however the rest of the context looks", () => {
      for (const windowFocused of [true, false]) {
        for (const desktopNotifications of [true, false, undefined]) {
          expect(
            decideDelivery(answer("convo-a"), ctx({ windowFocused, desktopNotifications })).inApp,
          ).toBe(false);
        }
      }
    });

    it.each(["modelLoaded", "modelUnloaded", "downloadFinished"] as const)(
      "is always shown for %s, even with the chat open",
      (kind) => {
        expect(decideDelivery(model(kind, "M"), ctx()).inApp).toBe(true);
        expect(decideDelivery(model(kind, "M"), ctx({ view: "translate", active: null })).inApp).toBe(true);
      },
    );
  });

  describe("the native notification", () => {
    it("is sent with the switch on and the window in the background", () => {
      expect(decideDelivery(model("modelLoaded", "M"), ctx()).native).toBe(true);
    });

    it("is sent for the answer in the open conversation when the window is in the background", () => {
      const delivery = decideDelivery(answer("convo-a"), ctx({ view: "chat", active: { id: "convo-a" } }));
      expect(delivery).toEqual({ inApp: false, native: true });
    });

    it("is not sent while OpenPal is the focused window", () => {
      for (const event of [answer(), model("modelLoaded", "M"), model("downloadFinished", "M")]) {
        expect(decideDelivery(event, ctx({ windowFocused: true })).native).toBe(false);
      }
    });

    it("is not sent with the switch off", () => {
      expect(decideDelivery(model("modelLoaded", "M"), ctx({ desktopNotifications: false })).native).toBe(false);
    });

    it("is not sent when the switch is missing (settings not loaded, or a row saved before it existed)", () => {
      expect(decideDelivery(model("modelLoaded", "M"), ctx({ desktopNotifications: undefined })).native).toBe(
        false,
      );
    });

    it("is not sent by a build with no OS notifications (Android, iOS)", () => {
      const none = { nativeNotifications: false };
      expect(decideDelivery(answer(), ctx({ ...none, active: null })).native).toBe(false);
      expect(decideDelivery(model("downloadFinished", "M"), ctx(none)).native).toBe(false);
    });

    it("does not depend on which conversation or view is open", () => {
      for (const view of ["chat", "translate"]) {
        for (const active of [null, { id: "convo-a" }, { id: "convo-b" }]) {
          expect(decideDelivery(answer("convo-a"), ctx({ view, active })).native).toBe(true);
        }
      }
    });
  });

  it("keeps the in-app answer and download alerts where there are no OS notifications", () => {
    const none = { nativeNotifications: false };
    expect(decideDelivery(answer(), ctx({ ...none, active: null })).inApp).toBe(true);
    expect(decideDelivery(model("downloadFinished", "M"), ctx(none)).inApp).toBe(true);
  });
});

describe("hasNativeNotifications", () => {
  it("is what the build reports", () => {
    expect(hasNativeNotifications({ nativeNotifications: true })).toBe(true);
    expect(hasNativeNotifications({ nativeNotifications: false })).toBe(false);
  });

  it("reads as yes until the build has said, the way every desktop build answers", () => {
    expect(hasNativeNotifications(null)).toBe(true);
    expect(hasNativeNotifications(undefined)).toBe(true);
    expect(hasNativeNotifications({})).toBe(true);
  });

  it("follows the build, not the platform name", () => {
    // An Android build that grew a backend would say so; nothing here reads
    // `isAndroid`.
    const androidWithBackend = { isAndroid: true, nativeNotifications: true };
    expect(hasNativeNotifications(androidWithBackend)).toBe(true);
  });
});

describe("engineModelName", () => {
  const info = (id: string, name: string) => ({ spec: { id, name } }) as unknown as ModelInfo;
  const models = [info("qwen3-4b", "Qwen3 4B"), info("custom-x-q4-k-m", "X Q4 K M"), info("blank", "")];

  it("is the model's display name, the same label as the sidebar pill", () => {
    expect(engineModelName(models, "qwen3-4b")).toBe("Qwen3 4B");
    expect(engineModelName(models, "custom-x-q4-k-m")).toBe("X Q4 K M");
  });

  it("is null for an id the list doesn't hold, so callers never show a raw id", () => {
    expect(engineModelName(models, "deleted-model")).toBeNull();
    expect(engineModelName([], "qwen3-4b")).toBeNull();
  });

  it("is null for a model with a blank name", () => {
    expect(engineModelName(models, "blank")).toBeNull();
  });
});
