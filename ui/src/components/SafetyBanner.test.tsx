import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import SafetyBanner from "./SafetyBanner";

const { openLink } = vi.hoisted(() => ({
  openLink: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/api", () => ({ api: { openLink } }));

describe("SafetyBanner", () => {
  beforeEach(() => {
    openLink.mockClear();
  });

  it("renders nothing when not visible", () => {
    const { container } = render(<SafetyBanner visible={false} onDismiss={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows supportive copy with an assertive alert role when visible, pointing to findahelpline.com rather than a specific number", () => {
    render(<SafetyBanner visible onDismiss={vi.fn()} />);
    const banner = screen.getByRole("alert");
    expect(banner).toHaveTextContent(/hard right now/i);
    expect(banner).toHaveTextContent(/findahelpline\.com/i);
  });

  it("opens findahelpline.com", async () => {
    const user = userEvent.setup();
    render(<SafetyBanner visible onDismiss={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /find a helpline/i }));
    expect(openLink).toHaveBeenCalledWith("https://findahelpline.com");
  });

  it("calls onDismiss when the dismiss button is clicked", async () => {
    const user = userEvent.setup();
    const onDismiss = vi.fn();
    render(<SafetyBanner visible onDismiss={onDismiss} />);
    await user.click(screen.getByRole("button", { name: /dismiss crisis support message/i }));
    expect(onDismiss).toHaveBeenCalledOnce();
  });
});
