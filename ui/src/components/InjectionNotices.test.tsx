import { afterEach, describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";
import i18n from "../i18n";
import enInjection from "../i18n/locales/en.injection.json";
import frInjection from "../i18n/locales/fr.injection.json";
import type { InjectionNotice, InjectionOutcome, InjectionSource } from "../lib/types";
import { InjectionNotices } from "./InjectionNotices";

const SOURCES: InjectionSource[] = [
  "webPage",
  "webSearch",
  "file",
  "commandOutput",
  "attachment",
  "projectConversation",
  "skill",
  "composer",
  "toolOutput",
];
const OUTCOMES: InjectionOutcome[] = ["continued", "stopped", "sent"];

/** A name the way the backend would label each source (or none). */
const NAMES: Partial<Record<InjectionSource, string>> = {
  webPage: "https://www.example.org/some/long/path?q=1",
  webSearch: "cheap flights",
  file: "/home/ada/notes/todo.md",
  commandOutput: "cat notes.txt",
  attachment: "report.pdf",
  skill: "/summarize",
};

function notice(overrides: Partial<InjectionNotice> = {}): InjectionNotice {
  return {
    source: "attachment",
    name: "report.pdf",
    outcome: "stopped",
    unchecked: false,
    ...overrides,
  };
}

/** The visible sentence of each rendered notice line, without the
 * screen-reader-only prefix. */
function lines(): string[] {
  return Array.from(document.querySelectorAll(".injection-notice-text")).map(
    (el) => el.textContent ?? "",
  );
}

describe("InjectionNotices", () => {
  afterEach(async () => {
    await i18n.changeLanguage("en");
  });

  it("gives every source and outcome a real sentence, never a raw key", () => {
    const all = SOURCES.flatMap((source) =>
      OUTCOMES.map((outcome) => notice({ source, name: NAMES[source], outcome })),
    );
    render(<InjectionNotices notices={all} />);
    const rendered = lines();
    expect(rendered).toHaveLength(SOURCES.length * OUTCOMES.length);
    for (const text of rendered) {
      expect(text).toMatch(/^Possible hidden instructions for the AI in .+\. .+\.$/);
      expect(text).not.toMatch(/\{\{|\b(where|outcome|notice|review)\./);
    }
    expect(rendered).toContain(
      `Possible hidden instructions for the AI in the file “report.pdf”. ${enInjection.outcome.stopped}`,
    );
    expect(rendered).toContain(
      `Possible hidden instructions for the AI in your message. ${enInjection.outcome.sent}`,
    );
    expect(rendered).toContain(
      `Possible hidden instructions for the AI in the skill /summarize. ${enInjection.outcome.continued}`,
    );
  });

  it("names a page by its host and a file by its base name", () => {
    render(
      <InjectionNotices
        notices={[
          notice({ source: "webPage", name: NAMES.webPage, outcome: "continued" }),
          notice({ source: "file", name: NAMES.file, outcome: "stopped" }),
          notice({ source: "webSearch", name: "x".repeat(80), outcome: "stopped" }),
        ]}
      />,
    );
    const [page, file, search] = lines();
    expect(page).toBe(
      `Possible hidden instructions for the AI in a web page the AI opened (example.org). ${enInjection.outcome.continued}`,
    );
    expect(file).toContain("a file the AI read (todo.md).");
    // Anything else is capped, so one long label can't swamp the line.
    expect(search).toContain(`web search results (${"x".repeat(59)}…).`);
  });

  it("falls back to a generic phrase for a source or outcome from a newer version", () => {
    render(
      <InjectionNotices
        notices={[
          { source: "somethingNew" as InjectionSource, outcome: "stopped", unchecked: false },
          notice({ outcome: "skipped" as InjectionOutcome }),
        ]}
      />,
    );
    const [unknownSource, unknownOutcome] = lines();
    expect(unknownSource).toBe(
      `Possible hidden instructions for the AI in something the AI was about to read. ${enInjection.outcome.stopped}`,
    );
    expect(unknownOutcome).toBe("Possible hidden instructions for the AI in the file “report.pdf”.");
  });

  it("says when a text couldn't be checked in time", () => {
    render(<InjectionNotices notices={[notice({ unchecked: true, outcome: "continued" })]} />);
    expect(lines()).toEqual([
      `Couldn't finish checking the file “report.pdf” for hidden instructions in time. ${enInjection.outcome.continued}`,
    ]);
  });

  it("is a labeled list whose lines start with a warning for screen readers", () => {
    render(
      <InjectionNotices notices={[notice(), notice({ source: "composer", outcome: "sent" })]} />,
    );
    const list = screen.getByRole("list", { name: "Safety notices for this reply" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent(/^Warning: Possible hidden instructions/);
    expect(items[0].querySelector("svg")).toHaveAttribute("aria-hidden", "true");
  });

  it("is translated in French", async () => {
    await i18n.changeLanguage("fr");
    render(<InjectionNotices notices={[notice()]} />);
    const where = frInjection.where.attachment.replace("{{name}}", "report.pdf");
    expect(lines()).toEqual([
      `${frInjection.found.replace("{{where}}", where)} ${frInjection.outcome.stopped}`,
    ]);
    expect(screen.getByRole("list", { name: frInjection.notice.listLabel })).toBeInTheDocument();
  });

  it("says a stopped text from an earlier message had already been read", () => {
    render(
      <InjectionNotices
        notices={[
          notice({ messageId: "u0" }),
          notice({ messageId: "u1" }),
          notice({ messageId: "u0", outcome: "continued" }),
        ]}
        turnMessageId="u1"
      />,
    );
    const where = enInjection.where.attachment.replace("{{name}}", "report.pdf");
    const found = enInjection.found.replace("{{where}}", where);
    expect(lines()).toEqual([
      `${found} ${enInjection.outcome.stoppedEarlier}`,
      `${found} ${enInjection.outcome.stopped}`,
      `${found} ${enInjection.outcome.continued}`,
    ]);
  });

  it("says it in French too", async () => {
    await i18n.changeLanguage("fr");
    render(<InjectionNotices notices={[notice({ messageId: "u0" })]} turnMessageId="u1" />);
    expect(lines()[0]).toContain(frInjection.outcome.stoppedEarlier);
  });
});
