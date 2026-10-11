import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import ProgressBar, { useProgressAria } from "./ProgressBar";

describe("ProgressBar", () => {
  it("exposes role=progressbar with aria-valuenow for a determinate value", () => {
    render(<ProgressBar value={0.42} label="Downloading Llama 3.1 8B" />);
    const bar = screen.getByRole("progressbar", { name: "Downloading Llama 3.1 8B" });
    expect(bar).toHaveAttribute("aria-valuenow", "42");
    expect(bar).toHaveAttribute("aria-valuemin", "0");
    expect(bar).toHaveAttribute("aria-valuemax", "100");
  });

  it("clamps out-of-range values into 0..100", () => {
    render(<ProgressBar value={1.5} label="Installing" />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
  });

  it("omits aria-valuenow/min/max for an indeterminate (null) value", () => {
    render(<ProgressBar value={null} label="Setting up" />);
    const bar = screen.getByRole("progressbar", { name: "Setting up" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(bar).not.toHaveAttribute("aria-valuemin");
    expect(bar).not.toHaveAttribute("aria-valuemax");
  });

  it("sets the inner bar's width from the fraction, and leaves it unset when indeterminate", () => {
    const { container, rerender } = render(
      <ProgressBar value={0.25} label="Downloading" className="track" barClassName="fill" />,
    );
    const fill = container.querySelector(".fill") as HTMLElement;
    expect(fill.style.width).toBe("25%");

    rerender(<ProgressBar value={null} label="Downloading" className="track" barClassName="fill" />);
    expect((container.querySelector(".fill") as HTMLElement).style.width).toBe("");
  });
});

describe("useProgressAria", () => {
  it("returns the same shape ProgressBar spreads, for callers like ProgressRing", () => {
    expect(useProgressAria(0.5, "Downloading")).toEqual({
      role: "progressbar",
      "aria-label": "Downloading",
      "aria-valuemin": 0,
      "aria-valuemax": 100,
      "aria-valuenow": 50,
    });
    expect(useProgressAria(null, "Downloading")).toEqual({
      role: "progressbar",
      "aria-label": "Downloading",
    });
  });
});
