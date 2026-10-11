import { isValidElement, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";

/** Flattens a (possibly highlighted, span-wrapped) React tree back to plain text. */
function textContent(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textContent(node.props.children);
  return "";
}

function languageLabel(className: string | undefined): string | null {
  const match = /language-(\S+)/.exec(className ?? "");
  return match ? match[1] : null;
}

const FENCE_LINE = /^ {0,3}(`{3,}|~{3,})/;
const STANDALONE_DISPLAY_MATH = /^ {0,3}\$\$(.+)\$\$ *$/;
const BLOCK_PREFIX = /^\s*(>|[-*+]\s|\d+[.)]\s|#)/;

/**
 * CommonMark's math extension only treats `$$` as *display* (centered, own
 * line) math when the delimiters each sit alone on their own line, like a
 * code fence — `$$formula$$` crammed onto one line parses as small inline
 * math instead (same rule GitHub's own markdown math rendering uses). Models
 * overwhelmingly emit a standalone equation as one `$$formula$$` line, so
 * without this it never gets the display treatment. Split such a line onto
 * three so it parses as display math; math mixed into running prose, inside
 * a list/quote/heading, or inside a fenced code block, is left untouched.
 */
function promoteStandaloneDisplayMath(text: string): string {
  if (!text.includes("$$")) return text;
  const lines = text.split("\n");
  // A fence only closes another of the *same character* and *at least as
  // long* — a shorter fence of the same character (e.g. a 3-backtick example
  // nested inside a 4-backtick wrapper) is just content, per CommonMark.
  let fence: { char: string; length: number } | null = null;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fenceMatch = FENCE_LINE.exec(line);
    if (fenceMatch) {
      const marker = fenceMatch[1];
      if (fence === null) {
        fence = { char: marker[0], length: marker.length };
      } else if (marker[0] === fence.char && marker.length >= fence.length) {
        fence = null;
      }
      continue;
    }
    if (fence !== null || BLOCK_PREFIX.test(line)) continue;
    const mathMatch = STANDALONE_DISPLAY_MATH.exec(line);
    if (mathMatch) lines[i] = `$$\n${mathMatch[1]}\n$$`;
  }
  return lines.join("\n");
}

const CopyIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="9" y="9" width="11" height="11" rx="2.5" />
    <path d="M5 15a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2" />
  </svg>
);

const CheckIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M20 6 9 17l-5-5" />
  </svg>
);

/** A fenced code block: a header (language + copy) above the highlighted `<pre><code>`. */
const CodeBlock: Components["pre"] = ({ children, node: _node, ...rest }) => {
  const { t } = useTranslation();
  const codeEl = isValidElement<{ className?: string }>(children) ? children : null;
  const lang = languageLabel(codeEl?.props.className);
  const code = textContent(children).replace(/\n$/, "");
  const [copied, setCopied] = useState(false);
  return (
    <div className="md-code">
      <div className="md-code-head">
        {lang && (
          <span className="md-code-lang" aria-label={t("markdown.languageLabel", { language: lang })}>
            {lang}
          </span>
        )}
        <button
          type="button"
          className="msg-action md-code-copy"
          title={t("markdown.copyCode")}
          onClick={async () => {
            if (await copyText(code)) {
              setCopied(true);
              setTimeout(() => setCopied(false), 1400);
            }
          }}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
          <span className="msg-action-label">{copied ? t("markdown.copied") : t("markdown.copy")}</span>
        </button>
      </div>
      <pre tabIndex={0} aria-label={t("markdown.codeScrollable")} {...rest}>
        {children}
      </pre>
    </div>
  );
};

const HTTP_LINK = /^https?:\/\//i;

/**
 * Markdown http(s) links open in the system browser, never inside the app's
 * own webview. Anything else (in-page fragments, relative links, `mailto:`
 * — react-markdown's defaultUrlTransform already strips other dangerous
 * schemes to "") is left to the browser's normal anchor handling instead of
 * being force-routed through the http(s)-only open_link command, where it
 * would silently no-op.
 */
const Anchor: Components["a"] = ({ href, children, node: _node, ...rest }) => (
  <a
    {...rest}
    href={href}
    onClick={(e) => {
      if (href && HTTP_LINK.test(href)) {
        e.preventDefault();
        void api.openLink(href);
      }
    }}
  >
    {children}
  </a>
);

/** Wide tables (common in model cards) scroll within the bubble instead of overflowing it. */
const TableWrap: Components["table"] = ({ children, node: _node, ...rest }) => {
  const { t } = useTranslation();
  return (
    <div className="md-table-wrap" tabIndex={0} aria-label={t("markdown.tableScrollable")}>
      <table {...rest}>{children}</table>
    </div>
  );
};

const components: Components = { pre: CodeBlock, a: Anchor, table: TableWrap };

/** Assistant replies, rendered like a well-set page rather than raw text. */
export default function Markdown({ children }: { children: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeHighlight, { detect: false }], rehypeKatex]}
        components={components}
      >
        {promoteStandaloneDisplayMath(children)}
      </ReactMarkdown>
    </div>
  );
}
