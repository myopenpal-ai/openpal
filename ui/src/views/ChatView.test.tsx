import { describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import { ReplyActivity } from "./ChatView";
import type { ChatStep } from "../lib/types";

vi.mock("../lib/api", () => ({ api: { openLink: vi.fn() } }));

const steps: ChatStep[] = [
  { type: "search", data: { query: "weather lyon" } },
  { type: "read", data: { url: "https://www.meteo.example/lyon", title: "Lyon forecast" } },
  {
    type: "toolCall",
    data: { toolId: "read_file", summary: "notes.txt", decision: "allowOnce", ok: true },
  },
];

describe("ReplyActivity", () => {
  it("stays collapsed while live, narrating the current action", () => {
    render(
      <ReplyActivity
        thinking="Let me look this up."
        steps={steps.slice(0, 1)}
        liveStatus="Searching the web for “weather lyon”"
      />,
    );
    const head = screen.getByRole("button", { name: /Searching the web for “weather lyon”/ });
    expect(head).toHaveAttribute("aria-expanded", "false");
    expect(head).toHaveTextContent("1 search");
    expect(screen.queryByText("Let me look this up.")).not.toBeInTheDocument();
  });

  it("is a plain status line when there is nothing to expand yet", () => {
    render(<ReplyActivity thinking="" steps={[]} liveStatus="Using read_file…" />);
    expect(screen.getByText("Using read_file…")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("summarizes a finished turn and expands to the reasoning and steps in order", async () => {
    render(<ReplyActivity thinking="Let me look this up." steps={steps} liveStatus={null} />);
    const head = screen.getByRole("button", { name: /Thoughts/ });
    expect(head).toHaveTextContent("1 search · 1 page read · 1 action");

    await userEvent.click(head);
    expect(head).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Let me look this up.")).toBeInTheDocument();
    const lines = within(screen.getByRole("list")).getAllByRole("listitem");
    expect(lines.map((li) => li.textContent)).toEqual([
      "Searched“weather lyon”",
      "ReadLyon forecast",
      "read_fileDone · notes.txt",
    ]);
  });

  it("titles a turn by what it did when the model didn't reason", () => {
    const { unmount } = render(
      <ReplyActivity thinking="" steps={steps.slice(0, 2)} liveStatus={null} />,
    );
    expect(screen.getByRole("button", { name: /Searched the web/ })).toBeInTheDocument();
    unmount();
    render(<ReplyActivity thinking="" steps={steps} liveStatus={null} />);
    expect(screen.getByRole("button", { name: /Agent tools/ })).toBeInTheDocument();
  });
});
