import { createContext, isValidElement, useCallback, useContext, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { checkLink, parseWebLink, type LinkCheck } from "../lib/linkSafety";
import LinkConfirmDialog from "./LinkConfirmDialog";

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

/** How a web link asks to be opened — through the confirmation dialog, or
 * straight away where the text is OpenPal's own (update notes). */
const OpenWebLink = createContext<(url: URL, text: string) => void>(() => {});

/**
 * Rendered Markdown never holds the model's address in a live `href`: no
 * engine can then prefetch its DNS or preconnect on hover, no native "Open
 * Link"/"Copy Link" menu reaches it, and a click can't navigate the webview.
 * An http(s) link is a focusable `role="link"` showing the real address as
 * its tooltip, opened only through `OpenWebLink`. Every other target —
 * `mailto:`, relative, protocol-relative `//host`, or the empty one
 * react-markdown's defaultUrlTransform leaves for `javascript:` — is inert
 * text; an empty `href` would otherwise navigate the app to itself and reload
 * it. Only in-page `#` fragments keep a real `href`.
 */
const Anchor: Components["a"] = ({ href, children, node: _node, ...rest }) => {
  const open = useContext(OpenWebLink);
  if (href?.startsWith("#")) {
    return (
      <a {...rest} href={href}>
        {children}
      </a>
    );
  }
  const url = parseWebLink(href);
  if (!url) {
    return (
      <a {...rest} className="md-link-inert">
        {children}
      </a>
    );
  }
  const activate = () => open(url, textContent(children));
  return (
    <a
      {...rest}
      role="link"
      tabIndex={0}
      className="md-link"
      title={url.href}
      onClick={(e) => {
        e.preventDefault();
        activate();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          activate();
        }
      }}
    >
      {children}
    </a>
  );
};

const ImageIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="3" y="3" width="18" height="18" rx="2.5" />
    <circle cx="9" cy="9" r="2" />
    <path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21" />
  </svg>
);

/**
 * Images never load: fetching one would send its address — which a reply
 * can fill with conversation text — to another site, whatever the CSP
 * allows. A placeholder with the alt text stands in, and `src` never
 * reaches the DOM.
 */
const Image: Components["img"] = ({ alt }) => {
  const { t } = useTranslation();
  const name = alt?.trim();
  const label = name ? t("markdown.image.notShownNamed", { alt: name }) : t("markdown.image.notShown");
  return (
    <span className="md-img-blocked" role="img" aria-label={label} title={t("markdown.image.why")}>
      <ImageIcon />
      {label}
    </span>
  );
};

/** Wide tables (common in model cards) scroll within the bubble instead of overflowing it. */
const TableWrap: Components["table"] = ({ children, node: _node, ...rest }) => {
  const { t } = useTranslation();
  return (
    <div className="md-table-wrap" tabIndex={0} aria-label={t("markdown.tableScrollable")}>
      <table {...rest}>{children}</table>
    </div>
  );
};

const components: Components = { pre: CodeBlock, a: Anchor, img: Image, table: TableWrap };

/**
 * Assistant replies, rendered like a well-set page rather than raw text.
 * `confirmLinks` (on by default) asks before a web link opens; turn it off
 * only for text OpenPal itself wrote, such as update notes.
 */
export default function Markdown({
  children,
  confirmLinks = true,
}: {
  children: string;
  confirmLinks?: boolean;
}) {
  // Held here rather than in each link, so a reply still streaming in can
  // re-render its links without closing the dialog.
  const [pending, setPending] = useState<LinkCheck | null>(null);
  const openWebLink = useCallback(
    (url: URL, text: string) => {
      if (confirmLinks) setPending(checkLink(url, text));
      else void api.openLink(url.href);
    },
    [confirmLinks],
  );
  return (
    <div className="md">
      <OpenWebLink.Provider value={openWebLink}>
        <ReactMarkdown
          remarkPlugins={[remarkGfm, remarkMath]}
          rehypePlugins={[[rehypeHighlight, { detect: false }], rehypeKatex]}
          components={components}
        >
          {promoteStandaloneDisplayMath(children)}
        </ReactMarkdown>
      </OpenWebLink.Provider>
      {pending && (
        <LinkConfirmDialog
          link={pending}
          onCancel={() => setPending(null)}
          onOpen={() => {
            setPending(null);
            void api.openLink(pending.href);
          }}
        />
      )}
    </div>
  );
}
