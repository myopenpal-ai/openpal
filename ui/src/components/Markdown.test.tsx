import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import Markdown from "./Markdown";

const { openLink, copyText } = vi.hoisted(() => ({
  openLink: vi.fn().mockResolvedValue(undefined),
  copyText: vi.fn().mockResolvedValue(true),
}));

vi.mock("../lib/api", () => ({ api: { openLink } }));
vi.mock("../lib/clipboard", () => ({ copyText }));

describe("Markdown", () => {
  beforeEach(() => {
    openLink.mockClear();
    copyText.mockClear();
  });

  it("renders GFM: bold, lists and tables", () => {
    render(
      <Markdown>{"**bold**\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |"}</Markdown>,
    );
    expect(screen.getByText("bold").tagName).toBe("STRONG");
    expect(screen.getByText("one").closest("ul")).toBeInTheDocument();
    expect(screen.getByRole("table")).toBeInTheDocument();
  });

  it("wraps a wide table in a keyboard-focusable scrollable container", () => {
    render(<Markdown>{"| a | b |\n|---|---|\n| 1 | 2 |"}</Markdown>);
    const wrap = screen.getByRole("table").closest(".md-table-wrap");
    expect(wrap).toBeInTheDocument();
    expect(wrap).toHaveAttribute("tabIndex", "0");
    expect(wrap).toHaveAccessibleName();
  });

  it("makes a fenced code block's <pre> keyboard-focusable for horizontal scrolling", () => {
    render(<Markdown>{"```ts\nconst x = 1;\n```"}</Markdown>);
    const pre = document.querySelector("pre");
    expect(pre).toHaveAttribute("tabIndex", "0");
    expect(pre).toHaveAccessibleName();
  });

  it("gives a fenced code block a language label and a working copy button", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"```ts\nconst x = 1;\n```"}</Markdown>);

    expect(screen.getByText("ts")).toBeInTheDocument();
    await user.click(screen.getByTitle("Copy code"));
    expect(copyText).toHaveBeenCalledWith("const x = 1;");
  });

  it("copies a multi-line, multi-token code block back out byte-for-byte", async () => {
    const user = userEvent.setup();
    const source = ['function greet(name) {', '  // say hello', '  return `hi, ${name}`;', '}'].join(
      "\n",
    );
    render(<Markdown>{"```js\n" + source + "\n```"}</Markdown>);

    await user.click(screen.getByTitle("Copy code"));
    expect(copyText).toHaveBeenCalledWith(source);
  });

  it("omits the language pill for an untagged fence, without crashing", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"```\nplain output\n```"}</Markdown>);

    expect(document.querySelector(".md-code-lang")).not.toBeInTheDocument();
    await user.click(screen.getByTitle("Copy code"));
    expect(copyText).toHaveBeenCalledWith("plain output");
  });

  it("does not crash on an unrecognized fence language", () => {
    render(<Markdown>{"```notalanguage\nsome text\n```"}</Markdown>);
    expect(screen.getByText("notalanguage")).toBeInTheDocument();
    expect(screen.getByText("some text")).toBeInTheDocument();
  });

  it("tolerates an unterminated fence while a reply is still streaming", () => {
    const chunks = ["```js\n", "```js\nconst x", "```js\nconst x = 1", "```js\nconst x = 1;\n```"];
    for (const chunk of chunks) {
      const { unmount } = render(<Markdown>{chunk}</Markdown>);
      unmount();
    }
  });

  it("does not wrap inline code in the code-block chrome", () => {
    render(<Markdown>{"before `inline` after"}</Markdown>);
    expect(screen.queryByTitle("Copy code")).not.toBeInTheDocument();
    expect(screen.getByText("inline").tagName).toBe("CODE");
  });

  it("syntax-highlights a bash fence", () => {
    render(<Markdown>{'```bash\necho "hi"\n```'}</Markdown>);
    expect(screen.getByText("bash")).toBeInTheDocument();
    expect(document.querySelector(".hljs-string")).toBeInTheDocument();
  });

  it("syntax-highlights a ts fence", () => {
    render(<Markdown>{"```ts\nconst x: number = 1;\n```"}</Markdown>);
    expect(document.querySelector(".hljs-keyword")).toBeInTheDocument();
  });

  it("syntax-highlights an html fence", () => {
    render(<Markdown>{'```html\n<div class="x">hi</div>\n```'}</Markdown>);
    expect(document.querySelector(".hljs-tag")).toBeInTheDocument();
    expect(document.querySelector(".hljs-attr")).toBeInTheDocument();
  });

  it("renders inline math with KaTeX", () => {
    render(<Markdown>{"The bound is $T_{max}$ per token."}</Markdown>);
    expect(document.querySelector(".katex")).toBeInTheDocument();
    expect(document.querySelector(".katex-display")).not.toBeInTheDocument();
  });

  it("renders block math with KaTeX in display mode", () => {
    render(<Markdown>{"$$\nT_{max} = \\frac{R_{io}}{Data_{token}}\n$$"}</Markdown>);
    expect(document.querySelector(".katex-display")).toBeInTheDocument();
  });

  // CommonMark's math extension only treats `$$` as display math when each
  // delimiter sits alone on its own line — `$$formula$$` crammed onto one
  // line is technically just inline math with a 2-char delimiter. Models
  // overwhelmingly emit a standalone equation this way (as does this task's
  // own example), so Markdown promotes a line that's *only* `$$...$$` to
  // display math before handing it to remark-math.
  it("promotes a standalone single-line $$...$$ to display math", () => {
    render(<Markdown>{"$$T_{max} = \\frac{R_{io}}{Data_{token}}$$"}</Markdown>);
    expect(document.querySelector(".katex-display")).toBeInTheDocument();
  });

  it("leaves $$...$$ mixed into running prose as inline math", () => {
    render(<Markdown>{"The bound is $$T_{max}$$ per token, roughly."}</Markdown>);
    expect(document.querySelector(".katex")).toBeInTheDocument();
    expect(document.querySelector(".katex-display")).not.toBeInTheDocument();
  });

  it("does not rewrite a $$...$$-looking line inside a fenced code block", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"```text\n$$not math$$\n```"}</Markdown>);
    expect(document.querySelector(".katex")).not.toBeInTheDocument();
    await user.click(screen.getByTitle("Copy code"));
    expect(copyText).toHaveBeenCalledWith("$$not math$$");
  });

  // A shorter fence of the *same* character nested inside a longer one (e.g. a
  // 3-backtick example inside a 4-backtick wrapper, common when a reply shows
  // markdown syntax) doesn't close the outer fence — so a $$...$$-looking line
  // in between is still code content, not a math line to rewrite.
  it("does not rewrite a $$...$$-looking line inside a nested (shorter) fence", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"````markdown\n```\n$$E=mc^2$$\n```\n````"}</Markdown>);
    expect(document.querySelector(".katex")).not.toBeInTheDocument();
    await user.click(screen.getByTitle("Copy code"));
    expect(copyText).toHaveBeenCalledWith("```\n$$E=mc^2$$\n```");
  });

  it("does not crash on invalid LaTeX", () => {
    render(<Markdown>{"$\\frac{1}{$"}</Markdown>);
    expect(document.querySelector(".md")).toBeInTheDocument();
  });

  it("tolerates unterminated display math while streaming", () => {
    const chunks = [
      "The bound is $$T",
      "The bound is $$T_{max",
      "The bound is $$T_{max} = \\frac{a}{b}$$",
    ];
    for (const chunk of chunks) {
      const { unmount } = render(<Markdown>{chunk}</Markdown>);
      unmount();
    }
  });

  it("opens http(s) links through api.openLink instead of navigating in-app", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com)"}</Markdown>);

    const link = screen.getByRole("link", { name: "go" });
    const clickEvent = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);

    await user.click(link);
    expect(openLink).toHaveBeenCalledWith("https://example.com");
  });

  it("leaves in-page fragment links to default browser handling", () => {
    render(<Markdown>{"[jump](#usage)"}</Markdown>);
    const link = screen.getByRole("link", { name: "jump" });

    const clickEvent = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(clickEvent);

    expect(clickEvent.defaultPrevented).toBe(false);
    expect(openLink).not.toHaveBeenCalled();
  });

  it("no-ops on an empty link destination", async () => {
    const user = userEvent.setup();
    const { container } = render(<Markdown>{"[x]()"}</Markdown>);
    const link = container.querySelector("a");
    expect(link).not.toBeNull();
    await user.click(link as Element);
    expect(openLink).not.toHaveBeenCalled();
  });
});
