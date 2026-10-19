/**
 * The rules for the four things OpenPal tells the user about — a model
 * loaded, a model unloaded, a download finished, an answer is ready — kept
 * apart from the store so each rule is a plain function with a plain test:
 * what the words are (`alertCopy`), and where they go (`decideDelivery`).
 *
 * Deliberately imports nothing from `state/store.ts`: the store imports this
 * file, and a cycle would leave one of them half-initialised at import time.
 * The store reads its own state and hands the parts these functions need.
 */

import i18n from "../i18n";

/** One occurrence of something worth telling the user. `key` is unique to the
 * occurrence (not to the kind), so a delivery that accidentally happens twice
 * can be recognised and shown once. */
export type AppEvent =
  | {
      kind: "modelLoaded" | "modelUnloaded" | "downloadFinished";
      key: string;
      /** The model's display name, or null if it can't be resolved (the copy
       * then says "Your AI" rather than showing a raw id). */
      modelName: string | null;
    }
  | {
      kind: "answerReady";
      key: string;
      /** The conversation the new answer belongs to. */
      conversationId: string;
    };

/** The words, already translated. An alert shows them, and the native
 * notification for the same event carries exactly the same two strings. */
export interface AppEventCopy {
  title: string;
  body: string;
}

export function alertCopy(event: AppEvent): AppEventCopy {
  const kind = event.kind;
  const name =
    event.kind === "answerReady" ? "" : event.modelName || i18n.t("notifications.unknownModel");
  return {
    title: i18n.t(`notifications.${kind}.title`),
    body: i18n.t(`notifications.${kind}.body`, { name }),
  };
}

/** The slice of the store `isConversationOpenInChat` needs. `view` is a plain
 * string so this file need not import the store's `View` type. */
export interface ChatLocation {
  view: string;
  active: { id: string } | null;
  /** The More dialog's page (Settings, Library, Plugins, ...), or null when it
   * is closed. It opens over the chat without changing `view`. */
  moreDialogPage: string | null;
  /** Conversation search is the other full-window dialog over the chat. */
  conversationSearchOpen: boolean;
}

/** Whether the user is looking at this conversation right now: the chat view
 * (not Translate, Correct or Improve) with this conversation open in it and
 * nothing laid over it. The More dialog and conversation search cover most of
 * the window while `view` stays "chat", so someone reading Settings or the
 * Library is not looking at the answer. */
export function isConversationOpenInChat(state: ChatLocation, conversationId: string): boolean {
  return (
    state.view === "chat" &&
    state.active?.id === conversationId &&
    state.moreDialogPage === null &&
    !state.conversationSearchOpen
  );
}

/** Whether this build can post OS notifications at all
 * (`AppInfo.nativeNotifications`). Until `get_app_info` has answered there's no
 * telling, so it reads as yes, the way every desktop build answers; Rust
 * re-checks before posting anything either way. */
export function hasNativeNotifications(
  appInfo: { nativeNotifications?: boolean } | null | undefined,
): boolean {
  return appInfo?.nativeNotifications !== false;
}

export interface DeliveryContext extends ChatLocation {
  /** `settings.desktopNotifications`; undefined while settings haven't loaded
   * or for a row saved before the field existed — both mean off. */
  desktopNotifications: boolean | undefined;
  /** Whether OpenPal's window has the OS's focus. */
  windowFocused: boolean;
  /** `hasNativeNotifications(appInfo)`. */
  nativeNotifications: boolean;
}

export interface Delivery {
  /** Show the in-app alert. */
  inApp: boolean;
  /** Also post an OS notification with the same words. */
  native: boolean;
}

/**
 * Where one event goes. The two answers are independent on purpose:
 *
 * - The in-app alert is skipped only for an answer whose conversation the
 *   user is already looking at — the answer is on screen, so a pop-up about it
 *   is noise.
 * - The native notification depends only on the switch and on OpenPal being in
 *   the background. It does NOT look at which conversation is open: with the
 *   window unfocused the user can't see the answer, open conversation or not.
 *   A build with no OS notifications (Android and iOS today) never posts one.
 */
export function decideDelivery(event: AppEvent, ctx: DeliveryContext): Delivery {
  return {
    inApp: event.kind !== "answerReady" || !isConversationOpenInChat(ctx, event.conversationId),
    native: ctx.desktopNotifications === true && !ctx.windowFocused && ctx.nativeNotifications,
  };
}
