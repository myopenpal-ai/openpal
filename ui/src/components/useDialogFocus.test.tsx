import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useCloseOnOutside } from "./Dropdown";
import { useDialogFocus } from "./useDialogFocus";

function Dialog({ onClose }: { onClose?: () => void }) {
  const ref = useDialogFocus<HTMLDivElement>(onClose);
  return (
    <div ref={ref} tabIndex={-1} role="dialog" aria-label="test dialog">
      <button>first</button>
      <button>middle</button>
      <button>last</button>
    </div>
  );
}

/** Mounts/unmounts `Dialog` the same way every real consumer does — the
 * parent conditionally renders it, rather than toggling a visibility prop. */
function Harness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button onClick={() => setOpen(true)}>trigger</button>
      {open && <Dialog onClose={() => setOpen(false)} />}
    </>
  );
}

describe("useDialogFocus", () => {
  it("moves focus into the dialog root on mount", () => {
    render(<Dialog />);
    expect(screen.getByRole("dialog")).toHaveFocus();
  });

  it("does not steal focus from a field that already grabbed it itself (e.g. autoFocus)", () => {
    function AutoFocusDialog() {
      const ref = useDialogFocus<HTMLDivElement>();
      return (
        <div ref={ref} tabIndex={-1} role="dialog">
          {/* eslint-disable-next-line jsx-a11y/no-autofocus */}
          <input autoFocus aria-label="name" />
        </div>
      );
    }
    render(<AutoFocusDialog />);
    expect(screen.getByLabelText("name")).toHaveFocus();
  });

  it("wraps Tab from the last focusable element back to the first", async () => {
    const user = userEvent.setup();
    render(<Dialog />);
    screen.getByText("last").focus();
    await user.tab();
    expect(screen.getByText("first")).toHaveFocus();
  });

  it("wraps Shift+Tab from the first focusable element back to the last", async () => {
    const user = userEvent.setup();
    render(<Dialog />);
    screen.getByText("first").focus();
    await user.tab({ shift: true });
    expect(screen.getByText("last")).toHaveFocus();
  });

  it("calls onClose on Escape when provided", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Dialog onClose={onClose} />);
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes only the innermost of two nested dialogs on Escape, and keeps it from document listeners", async () => {
    const user = userEvent.setup();
    const outerClose = vi.fn();
    const onDocumentEscape = vi.fn();
    function Outer() {
      const ref = useDialogFocus<HTMLDivElement>(outerClose);
      const [innerOpen, setInnerOpen] = useState(true);
      return (
        <div ref={ref} tabIndex={-1} role="dialog" aria-label="outer">
          {innerOpen && <Dialog onClose={() => setInnerOpen(false)} />}
        </div>
      );
    }
    const onKeyDown = (e: KeyboardEvent) => e.key === "Escape" && onDocumentEscape();
    document.addEventListener("keydown", onKeyDown);
    try {
      render(<Outer />);
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("dialog", { name: "test dialog" })).not.toBeInTheDocument();
      expect(screen.getByRole("dialog", { name: "outer" })).toBeInTheDocument();
      expect(outerClose).not.toHaveBeenCalled();
      expect(onDocumentEscape).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("keydown", onKeyDown);
    }
  });

  it("leaves Escape to a popover opened from inside it, closing only that", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    function WithPopover() {
      const ref = useDialogFocus<HTMLDivElement>(onClose);
      const [open, setOpen] = useState(false);
      const panelRef = useCloseOnOutside<HTMLDivElement>(open, () => setOpen(false));
      return (
        <div ref={ref} tabIndex={-1} role="dialog" aria-label="outer">
          <button aria-expanded={open} onClick={() => setOpen(true)}>
            info
          </button>
          {open && (
            <div ref={panelRef} role="note">
              explainer
            </div>
          )}
        </div>
      );
    }
    render(<WithPopover />);
    await user.click(screen.getByText("info"));
    expect(screen.getByRole("note")).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();

    // With the popover gone, Escape is the dialog's again.
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does nothing on Escape when onClose is omitted", async () => {
    const user = userEvent.setup();
    render(<Dialog />);
    await expect(user.keyboard("{Escape}")).resolves.not.toThrow();
  });

  it("restores focus to the pre-open element once the dialog unmounts", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const trigger = screen.getByText("trigger");
    await user.click(trigger);
    expect(screen.getByRole("dialog")).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
  });

  it("restores focus to the opener even when a field inside grabbed focus first", async () => {
    const user = userEvent.setup();
    function AutoFocusDialog({ onClose }: { onClose: () => void }) {
      const ref = useDialogFocus<HTMLDivElement>(onClose);
      return (
        <div ref={ref} tabIndex={-1} role="dialog">
          {/* eslint-disable-next-line jsx-a11y/no-autofocus */}
          <input autoFocus aria-label="name" />
        </div>
      );
    }
    function AutoFocusHarness() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button onClick={() => setOpen(true)}>trigger</button>
          {open && <AutoFocusDialog onClose={() => setOpen(false)} />}
        </>
      );
    }
    render(<AutoFocusHarness />);
    const trigger = screen.getByText("trigger");
    await user.click(trigger);
    expect(screen.getByLabelText("name")).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
  });

  it("restores focus to what opened it when the dialog's component stays mounted between openings", async () => {
    const user = userEvent.setup();
    // SkillsView's shape: the hook lives in an always-mounted page, and only
    // the dialog markup (with an autoFocus field) comes and goes.
    function Page() {
      const [open, setOpen] = useState(false);
      const ref = useDialogFocus<HTMLDivElement>(() => setOpen(false));
      return (
        <>
          <button>elsewhere</button>
          <button onClick={() => setOpen(true)}>new</button>
          {open && (
            <div ref={ref} tabIndex={-1} role="dialog">
              {/* eslint-disable-next-line jsx-a11y/no-autofocus */}
              <input autoFocus aria-label="name" />
            </div>
          )}
        </>
      );
    }
    render(<Page />);
    await user.click(screen.getByText("elsewhere"));
    await user.click(screen.getByText("new"));
    expect(screen.getByLabelText("name")).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.getByText("new")).toHaveFocus();
  });

  it("gives focus back to a clicked opener that never took focus itself (WebKit)", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const trigger = screen.getByText("trigger");
    // WebKit's mouse click: pointer events and a click, but focus stays put.
    fireEvent.pointerDown(trigger);
    fireEvent.click(trigger);
    expect(screen.getByRole("dialog")).toHaveFocus();

    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
  });

  it("still moves focus in once real content appears, for a component that stays mounted through an earlier null render (e.g. UpdateDialog's 'checking' phase)", async () => {
    const user = userEvent.setup();
    function StaysMountedThenRendersContent() {
      const [ready, setReady] = useState(false);
      const ref = useDialogFocus<HTMLDivElement>();
      if (!ready) {
        // Same component instance, same hook call, no real DOM yet — the
        // callback ref hasn't attached to anything.
        return <button onClick={() => setReady(true)}>become ready</button>;
      }
      return (
        <div ref={ref} tabIndex={-1} role="dialog">
          <button>inside</button>
        </div>
      );
    }
    render(<StaysMountedThenRendersContent />);
    await user.click(screen.getByText("become ready"));
    expect(screen.getByRole("dialog")).toHaveFocus();
  });

  it("does not re-steal focus when the parent re-renders with a fresh onClose closure", () => {
    function Rerendering() {
      const [n, setN] = useState(0);
      const ref = useDialogFocus<HTMLDivElement>(() => {});
      return (
        <div>
          <div ref={ref} tabIndex={-1} role="dialog">
            <button>inside</button>
          </div>
          <button onClick={() => setN((x) => x + 1)}>rerender {n}</button>
        </div>
      );
    }
    render(<Rerendering />);
    const inside = screen.getByText("inside");
    inside.focus();
    expect(inside).toHaveFocus();
    screen.getByText(/rerender/).click();
    // A naive effect keyed on `onClose` would re-run here (fresh closure
    // each render) and yank focus back to the dialog root.
    expect(inside).toHaveFocus();
  });
  describe("unprompted", () => {
    function Card({ show }: { show: boolean }) {
      const ref = useDialogFocus<HTMLDivElement>(undefined, { unprompted: true });
      return show ? (
        <div ref={ref} tabIndex={-1} role="alertdialog" aria-label="card">
          <button>answer</button>
        </div>
      ) : null;
    }

    it("takes focus when nothing else holds it, like any dialog", () => {
      render(<Card show />);
      expect(screen.getByRole("alertdialog")).toHaveFocus();
    });

    it("leaves focus in a draft, and leaves it there once it closes", async () => {
      const user = userEvent.setup();
      const { rerender } = render(
        <>
          <input aria-label="draft" defaultValue="half a sentence" />
          <Card show={false} />
        </>,
      );
      const draft = screen.getByLabelText("draft");
      draft.focus();
      rerender(
        <>
          <input aria-label="draft" defaultValue="half a sentence" />
          <Card show />
        </>,
      );
      expect(draft).toHaveFocus();
      await user.type(draft, " more");
      rerender(
        <>
          <input aria-label="draft" defaultValue="half a sentence" />
          <Card show={false} />
        </>,
      );
      expect(draft).toHaveFocus();
      expect(draft).toHaveValue("half a sentence more");
    });

    it("leaves focus in another dialog", () => {
      render(<Dialog />);
      const other = screen.getByRole("dialog");
      expect(other).toHaveFocus();
      render(<Card show />);
      expect(other).toHaveFocus();
    });
  });
});
