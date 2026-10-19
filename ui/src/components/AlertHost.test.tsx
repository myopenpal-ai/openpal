import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import AlertHost, { ALERT_DURATION_MS } from "./AlertHost";
import { MAX_VISIBLE_ALERTS, useStore, type AppAlert } from "../state/store";

vi.mock("../lib/api", () => ({ api: {} }));

type NewAlert = Omit<AppAlert, "id">;

const loaded: NewAlert = {
  kind: "modelLoaded",
  title: "Model ready",
  body: "Qwen is loaded and ready to chat.",
};
const answer: NewAlert = {
  kind: "answerReady",
  title: "Answer ready",
  body: "OpenPal has finished answering.",
};

/** Pushes an alert the way the app does, inside `act` so React flushes it. */
function show(alert: NewAlert = loaded) {
  act(() => useStore.getState().showAlert(alert));
}

/** Moves the fake clock and lets React apply whatever the timers did. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

/** Counts the Escape key presses that reach `target`'s bubble-phase listeners:
 * the stand-in for the document and window listeners that close menus,
 * previews and the lightbox. (Tab presses from `user.tab()` pass through too,
 * so only Escape is counted.) */
function escapesHeardOn(target: Window | Document) {
  let heard = 0;
  const listener = (e: Event) => {
    if ((e as KeyboardEvent).key === "Escape") heard += 1;
  };
  target.addEventListener("keydown", listener);
  return { count: () => heard, stop: () => target.removeEventListener("keydown", listener) };
}

function dismissButton(title: string): HTMLElement {
  return screen.getByRole("button", { name: `Dismiss ${title}` });
}

describe("AlertHost", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Testing Library drains its async wrapper with a setTimeout that only
    // fake timers *it recognises as Jest's* get advanced, so under Vitest every
    // `await user.…` would wait on a clock nobody moves. Presenting the one
    // method it calls makes it advance Vitest's clock instead.
    vi.stubGlobal("jest", { advanceTimersByTime: (ms: number) => vi.advanceTimersByTime(ms) });
    useStore.setState({ appAlerts: [] });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
    vi.restoreAllMocks();
    useStore.setState({ appAlerts: [] });
  });

  /** user-event drives its own delays through the (fake) clock. */
  const setup = () => userEvent.setup({ advanceTimers: vi.advanceTimersByTime });

  describe("announcing", () => {
    it("has its live region before any alert and fills that same node in place", () => {
      render(<AlertHost />);
      // Assistive tech registers a region when it enters the DOM, so it has to
      // be there, empty, before the first alert exists.
      const status = screen.getByRole("status");
      expect(status).toBeEmptyDOMElement();
      expect(status).toHaveAttribute("aria-live", "polite");
      expect(status).toHaveAttribute("aria-atomic", "false");
      expect(status).toHaveAttribute("aria-relevant", "additions");

      show();
      expect(screen.getByRole("status")).toBe(status);
      expect(status).toHaveTextContent("Model ready. Qwen is loaded and ready to chat.");
    });

    it("renders no role=alert anywhere: nothing is announced twice or assertively", () => {
      render(<AlertHost />);
      show();
      show(answer);
      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.getAllByRole("status")).toHaveLength(1);
    });

    it("announces two identical alerts in a row as two lines", () => {
      render(<AlertHost />);
      show();
      show();
      const lines = within(screen.getByRole("status")).getAllByText(/Model ready/);
      expect(lines).toHaveLength(2);
    });

    it("drops an alert's line from the region when the alert goes", () => {
      render(<AlertHost />);
      show();
      advance(ALERT_DURATION_MS);
      expect(screen.getByRole("status")).toBeEmptyDOMElement();
    });

    it("labels the stack as a group only while it holds something", () => {
      render(<AlertHost />);
      expect(screen.queryByRole("group")).toBeNull();
      show();
      expect(screen.getByRole("group", { name: "Notifications" })).toBeInTheDocument();
    });

    it("gives every Dismiss button its alert's title in its name", () => {
      render(<AlertHost />);
      show();
      show(answer);
      expect(dismissButton("Model ready")).toBeInTheDocument();
      expect(dismissButton("Answer ready")).toBeInTheDocument();
    });
  });

  describe("timing", () => {
    it("clears itself after ALERT_DURATION_MS and leaves no timer behind", () => {
      render(<AlertHost />);
      show();
      advance(ALERT_DURATION_MS - 1);
      expect(screen.getByText("Model ready", { selector: "[data-slot=alert-title]" })).toBeInTheDocument();
      advance(1);
      expect(screen.queryByText("Model ready", { selector: "[data-slot=alert-title]" })).toBeNull();
      expect(useStore.getState().appAlerts).toEqual([]);
      expect(vi.getTimerCount()).toBe(0);
    });

    it("waits while the pointer is over the stack and resumes when it leaves", async () => {
      const user = setup();
      render(<AlertHost />);
      show();
      await user.hover(dismissButton("Model ready"));
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(1);
      await user.unhover(dismissButton("Model ready"));
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("keeps what was left when it paused, rather than starting over", async () => {
      const user = setup();
      render(<AlertHost />);
      show();
      advance(4000);
      await user.hover(dismissButton("Model ready"));
      advance(60_000);
      await user.unhover(dismissButton("Model ready"));
      advance(ALERT_DURATION_MS - 4000 - 1);
      expect(useStore.getState().appAlerts).toHaveLength(1);
      advance(1);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("does not treat a touch as a hover, which never gets its matching leave", () => {
      render(<AlertHost />);
      show();
      fireEvent.pointerOver(dismissButton("Model ready"), { pointerType: "touch" });
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("waits while focus is inside the stack", async () => {
      const user = setup();
      render(<AlertHost />);
      show();
      await user.tab();
      expect(dismissButton("Model ready")).toHaveFocus();
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(1);
      await user.tab();
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("does not stay frozen after the alert that held focus is removed by something else", async () => {
      // Eviction by a newer alert, say: the focused button is deleted from the
      // DOM and the browser sends no blur, so focus-within has to be re-read.
      const user = setup();
      render(<AlertHost />);
      show();
      await user.tab();
      expect(dismissButton("Model ready")).toHaveFocus();
      act(() => useStore.getState().dismissAlert(useStore.getState().appAlerts[0].id));
      expect(document.body).toHaveFocus();
      show(answer);
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("waits while the window is in the background and resumes with what was left", () => {
      render(<AlertHost />);
      show();
      advance(3000);
      act(() => {
        fireEvent.blur(window);
      });
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(1);
      act(() => {
        fireEvent.focus(window);
      });
      advance(ALERT_DURATION_MS - 3000 - 1);
      expect(useStore.getState().appAlerts).toHaveLength(1);
      advance(1);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("an alert that arrives while the window is in the background waits for it too", () => {
      render(<AlertHost />);
      act(() => {
        fireEvent.blur(window);
      });
      show();
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(1);
    });

    it("does not stay frozen after the hovered alert is clicked away", async () => {
      // The pointer is on the button when the last alert goes, so the stack
      // never sees a pointerleave. The next alert must still count down.
      const user = setup();
      render(<AlertHost />);
      show();
      await user.click(dismissButton("Model ready"));
      expect(useStore.getState().appAlerts).toHaveLength(0);
      show(answer);
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
    });

    it("stops every timer on unmount", () => {
      const { unmount } = render(<AlertHost />);
      show();
      show(answer);
      expect(vi.getTimerCount()).toBeGreaterThan(0);
      unmount();
      expect(vi.getTimerCount()).toBe(0);
    });

    it("expires exactly once under StrictMode's double mount", () => {
      const real = useStore.getState().dismissAlert;
      const dismissSpy = vi.fn(real);
      useStore.setState({ dismissAlert: dismissSpy });
      try {
        render(
          <StrictMode>
            <AlertHost />
          </StrictMode>,
        );
        show();
        const [{ id }] = useStore.getState().appAlerts;
        advance(ALERT_DURATION_MS - 1);
        expect(dismissSpy).not.toHaveBeenCalled();
        advance(1);
        expect(dismissSpy).toHaveBeenCalledTimes(1);
        expect(dismissSpy).toHaveBeenCalledWith(id);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        useStore.setState({ dismissAlert: real });
      }
    });
  });

  describe("focus", () => {
    it("never steals focus from the composer when an alert appears or expires", () => {
      const focusSpy = vi.spyOn(HTMLElement.prototype, "focus");
      render(
        <>
          <textarea aria-label="Message" />
          <AlertHost />
        </>,
      );
      const textarea = screen.getByRole("textbox", { name: "Message" });
      textarea.focus();
      focusSpy.mockClear();

      show();
      show(answer);
      expect(textarea).toHaveFocus();
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
      expect(textarea).toHaveFocus();
      expect(focusSpy).not.toHaveBeenCalled();
    });

    it("leaves focus in the composer when an alert is clicked or dismissed with the mouse", async () => {
      const user = setup();
      render(
        <>
          <textarea aria-label="Message" />
          <AlertHost />
        </>,
      );
      const textarea = screen.getByRole("textbox", { name: "Message" });
      show();
      show(answer);
      await user.click(textarea);
      expect(textarea).toHaveFocus();

      // On the card's text, then on a Dismiss button.
      await user.click(screen.getByText("OpenPal has finished answering."));
      expect(textarea).toHaveFocus();
      await user.click(dismissButton("Answer ready"));
      expect(useStore.getState().appAlerts.map((a) => a.title)).toEqual(["Model ready"]);
      expect(textarea).toHaveFocus();
    });

    it("keeps the same DOM nodes, and focus, when a newer alert arrives above", async () => {
      const user = setup();
      render(<AlertHost />);
      show();
      await user.tab();
      const button = dismissButton("Model ready");
      expect(button).toHaveFocus();

      show(answer);
      expect(dismissButton("Model ready")).toBe(button);
      expect(button).toHaveFocus();
    });
  });

  describe("keyboard", () => {
    it("reaches Dismiss with Tab from the composer, and Enter dismisses", async () => {
      const user = setup();
      render(
        <>
          <textarea aria-label="Message" />
          <AlertHost />
        </>,
      );
      const textarea = screen.getByRole("textbox", { name: "Message" });
      show();
      await user.click(textarea);
      await user.tab();
      expect(dismissButton("Model ready")).toHaveFocus();

      await user.keyboard("{Enter}");
      expect(useStore.getState().appAlerts).toHaveLength(0);
      // Focus goes back to where the Tab started instead of falling to <body>.
      expect(textarea).toHaveFocus();
    });

    it("dismisses the focused alert on Escape and hands focus on, newest to oldest, then home", async () => {
      const user = setup();
      render(
        <>
          <textarea aria-label="Message" />
          <AlertHost />
        </>,
      );
      const textarea = screen.getByRole("textbox", { name: "Message" });
      show();
      show(answer);
      await user.click(textarea);
      await user.tab();
      expect(dismissButton("Answer ready")).toHaveFocus();

      await user.keyboard("{Escape}");
      expect(useStore.getState().appAlerts.map((a) => a.title)).toEqual(["Model ready"]);
      expect(dismissButton("Model ready")).toHaveFocus();

      await user.keyboard("{Escape}");
      expect(useStore.getState().appAlerts).toHaveLength(0);
      expect(textarea).toHaveFocus();
    });

    it("hands focus to the previous alert when the last one in the stack is dismissed", async () => {
      const user = setup();
      render(<AlertHost />);
      show();
      show(answer);
      await user.tab();
      await user.tab();
      expect(dismissButton("Model ready")).toHaveFocus();
      await user.keyboard("{Escape}");
      expect(useStore.getState().appAlerts.map((a) => a.title)).toEqual(["Answer ready"]);
      expect(dismissButton("Answer ready")).toHaveFocus();
    });

    it("ignores Escape pressed elsewhere, and does not swallow it", async () => {
      const user = setup();
      const heard = escapesHeardOn(window);
      try {
        render(
          <>
            <textarea aria-label="Message" />
            <AlertHost />
          </>,
        );
        show();
        await user.click(screen.getByRole("textbox", { name: "Message" }));
        await user.keyboard("{Escape}");
        expect(useStore.getState().appAlerts).toHaveLength(1);
        // Whatever else listens for Escape (a preview, a menu) still hears it.
        expect(heard.count()).toBe(1);
      } finally {
        heard.stop();
      }
    });

    it("consumes Escape pressed inside the stack: nothing above it hears the key", async () => {
      const user = setup();
      const onWindow = escapesHeardOn(window);
      const onDocument = escapesHeardOn(document);
      try {
        render(<AlertHost />);
        show();
        await user.tab();
        await user.keyboard("{Escape}");
        expect(useStore.getState().appAlerts).toHaveLength(0);
        expect(onWindow.count()).toBe(0);
        expect(onDocument.count()).toBe(0);
      } finally {
        onWindow.stop();
        onDocument.stop();
      }
    });
  });

  describe("stacking", () => {
    it("shows at most MAX_VISIBLE_ALERTS, newest first, evicting the oldest", () => {
      const { container } = render(<AlertHost />);
      for (let n = 1; n <= MAX_VISIBLE_ALERTS + 1; n++) {
        show({ kind: "downloadFinished", title: `Alert ${n}`, body: `Body ${n}` });
      }
      const titles = Array.from(container.querySelectorAll("[data-slot=alert-title]")).map(
        (el) => el.textContent,
      );
      expect(MAX_VISIBLE_ALERTS).toBe(3);
      expect(titles).toEqual(["Alert 4", "Alert 3", "Alert 2"]);
      expect(within(screen.getByRole("status")).getAllByText(/Alert \d/)).toHaveLength(3);
    });

    it("draws the icon as a direct, hidden child of the card for every kind", () => {
      const { container } = render(<AlertHost />);
      for (const kind of ["modelLoaded", "modelUnloaded", "downloadFinished", "answerReady"] as const) {
        show({ kind, title: kind, body: "x" });
      }
      const cards = container.querySelectorAll("[data-slot=alert]");
      expect(cards).toHaveLength(MAX_VISIBLE_ALERTS);
      for (const card of cards) {
        const icon = card.querySelector(":scope > svg");
        expect(icon).not.toBeNull();
        expect(icon).toHaveAttribute("aria-hidden", "true");
      }
    });
  });

  describe("what a browser does not tell it", () => {
    it("lets the stack go on when the pointer is no longer over it after the list shrinks", () => {
      // Chromium sends no pointerleave when the card under a stationary pointer
      // is removed, so the host asks the browser (`:hover`) one frame later.
      const matches = vi.spyOn(Element.prototype, "matches").mockImplementation(function (
        this: Element,
        selector: string,
      ) {
        return selector === ":hover" ? false : Element.prototype.matches.call(this, selector);
      });
      render(<AlertHost />);
      show(loaded);
      show(answer);
      const stack = document.querySelector(".alert-host")!;
      fireEvent.pointerEnter(stack, { pointerType: "mouse" });
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(2);

      // The lower card is clicked away; the pointer now rests on empty space.
      act(() => useStore.getState().dismissAlert(useStore.getState().appAlerts[1].id));
      advance(20); // a frame
      advance(ALERT_DURATION_MS);
      expect(useStore.getState().appAlerts).toHaveLength(0);
      matches.mockRestore();
    });

    it("keeps the hold when the pointer is still over the stack after the list changes", () => {
      const matches = vi.spyOn(Element.prototype, "matches").mockImplementation(function (
        this: Element,
        selector: string,
      ) {
        return selector === ":hover" ? true : Element.prototype.matches.call(this, selector);
      });
      render(<AlertHost />);
      show(loaded);
      fireEvent.pointerEnter(document.querySelector(".alert-host")!, { pointerType: "mouse" });
      show(answer); // a newer alert arrives under a resting pointer
      advance(20);
      advance(60_000);
      expect(useStore.getState().appAlerts).toHaveLength(2);
      matches.mockRestore();
    });

    it("hands focus on when the card that held it is pushed out by a newer alert", async () => {
      const user = setup();
      render(<AlertHost />);
      show({ ...loaded, title: "One" });
      show({ ...loaded, title: "Two" });
      show({ ...loaded, title: "Three" });
      expect(useStore.getState().appAlerts).toHaveLength(MAX_VISIBLE_ALERTS);
      // Newest first, so the oldest, "One", is the third Tab stop.
      await user.tab();
      await user.tab();
      await user.tab();
      expect(dismissButton("One")).toHaveFocus();

      show({ ...loaded, title: "Four" });

      expect(screen.queryByRole("button", { name: "Dismiss One" })).toBeNull();
      // Not dropped on <body>, where the next Tab would restart at the skip link.
      expect(document.body).not.toHaveFocus();
      expect(dismissButton("Two")).toHaveFocus();
    });

    it("remembers where focus came from across the window losing and regaining it", async () => {
      const user = setup();
      render(
        <>
          <textarea aria-label="Message" />
          <AlertHost />
        </>,
      );
      const composer = screen.getByRole("textbox", { name: "Message" });
      composer.focus();
      show();
      await user.tab();
      expect(dismissButton("Model ready")).toHaveFocus();

      // The OS deactivates and reactivates the window: the same element is
      // focused again, with no relatedTarget.
      const button = dismissButton("Model ready");
      act(() => button.blur());
      act(() => button.focus());

      await user.keyboard("{Escape}");
      expect(useStore.getState().appAlerts).toHaveLength(0);
      expect(composer).toHaveFocus();
    });

    it("does not let a press on an alert count as a click outside an open popover", () => {
      const outside = vi.fn();
      document.addEventListener("mousedown", outside);
      render(<AlertHost />);
      show();
      fireEvent.mouseDown(screen.getByText("Model ready"));
      fireEvent.mouseDown(dismissButton("Model ready"));
      document.removeEventListener("mousedown", outside);
      expect(outside).not.toHaveBeenCalled();
    });
  });

  // The slice the host stands on; store.test.ts covers the rest of the store.
  describe("appAlerts in the store", () => {
    it("puts the newest first, gives each a distinct id and caps the list", () => {
      const { showAlert } = useStore.getState();
      for (let n = 1; n <= 5; n++) showAlert({ ...loaded, title: `T${n}` });
      const alerts = useStore.getState().appAlerts;
      expect(alerts.map((a) => a.title)).toEqual(["T5", "T4", "T3"]);
      expect(new Set(alerts.map((a) => a.id)).size).toBe(3);
    });

    it("dismisses by id, and a missing id changes nothing (not even the state object)", () => {
      useStore.getState().showAlert(loaded);
      useStore.getState().showAlert(answer);
      const [newest, oldest] = useStore.getState().appAlerts;
      useStore.getState().dismissAlert(newest.id);
      expect(useStore.getState().appAlerts).toEqual([oldest]);

      const before = useStore.getState();
      useStore.getState().dismissAlert(newest.id);
      expect(useStore.getState()).toBe(before);
    });
  });
});
