import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { useState } from "react";
import "../i18n";
import { AssistantBody } from "./ChatView";

// The regression this guards: ChatView's message list re-renders once per
// streamed token, and only the live reply's `content` actually changes. If
// `AssistantBody` (or `Markdown`) is not memoized, every earlier, settled
// reply on screen re-parses its full markdown through GFM/math/highlight/
// katex on every token — the render cost scales with the conversation and
// stalls the live reply. `react-markdown` is replaced here by a recorder so
// the test counts how many times each message's markdown is (re)parsed.
const md = vi.hoisted(() => {
  const counts = new Map<string, number>();
  const record = (text: unknown) => {
    const key = String(text ?? "");
    counts.set(key, (counts.get(key) ?? 0) + 1);
  };
  return { counts, record };
});

vi.mock("react-markdown", () => ({
  default: ({ children }: { children?: unknown }) => {
    md.record(children);
    return <div data-testid="md" />;
  },
}));
vi.mock("../lib/api", () => ({ api: { openLink: vi.fn() } }));
vi.mock("../lib/clipboard", () => ({ copyText: vi.fn().mockResolvedValue(true) }));

/** Earlier, already-settled replies on screen — the "the more, the worse"
 *  half of the ticket. Code-containing so a re-parse would be as costly as
 *  in the real app. */
const EARLIER_REPLIES = [
  "Earlier answer with `inline` code.",
  "An earlier code reply:\n\n```ts\nconst y = 2;\n```",
];

/** The streaming reply's tokens, emitted one per `token` event. */
const STREAM_TOKENS = ["```ts\n", "const x: number ", "= 1;\nconsole.log(x);\n", "```"];

/** Hands the test a way to push one token at a time, mirroring how
 *  `runGeneration`'s `token` handler appends a delta and re-renders. */
let pushToken: (token: string) => void = () => undefined;

/** Behave like ChatView's message list: several settled AssistantBody
 *  replies, then the live one whose `content` grows once per token. */
function StreamingList() {
  const [tokens, setTokens] = useState<string[]>([]);
  pushToken = (token: string) => setTokens((current) => [...current, token]);
  const liveContent = tokens.join("");
  return (
    <div>
      {EARLIER_REPLIES.map((content, i) => (
        <AssistantBody key={`earlier-${i}`} content={content} isStreamingReply={false} />
      ))}
      <AssistantBody content={liveContent} isStreamingReply />
    </div>
  );
}

const earlierRenders = () =>
  EARLIER_REPLIES.map((content) => md.counts.get(content) ?? 0);

const liveRenders = () => {
  let total = 0;
  for (const [content, count] of md.counts) {
    if (!EARLIER_REPLIES.includes(content)) total += count;
  }
  return total;
};

describe("AssistantBody per-token reply updates", () => {
  beforeEach(() => {
    md.counts.clear();
    pushToken = () => undefined;
  });

  it("re-parses only the live reply's markdown as tokens arrive", async () => {
    render(<StreamingList />);

    // Mount: every settled reply and the empty live one parsed once each.
    expect(earlierRenders()).toEqual([1, 1]);

    for (const token of STREAM_TOKENS) {
      await act(async () => {
        pushToken(token);
      });
    }
    // Flush any deferred catch-up render of the final content.
    await act(async () => {});

    // A settled reply's markdown is parsed exactly once for the whole run.
    // Before the memo fix it is re-parsed once per token (1 + 4 = 5 each).
    expect(earlierRenders()).toEqual([1, 1]);

    // The live reply kept rendering its markdown as each token landed —
    // visible token-by-token growth rather than a freeze then a dump — and
    // the full text arrived intact: every intermediate chunk and the final,
    // complete reply were rendered at least once.
    expect(liveRenders()).toBeGreaterThanOrEqual(STREAM_TOKENS.length);
    for (let i = 1; i <= STREAM_TOKENS.length; i++) {
      expect(md.counts.get(STREAM_TOKENS.slice(0, i).join("")) ?? 0).toBeGreaterThanOrEqual(1);
    }
  });

  it("keeps an untouched earlier reply cost-free under a long stream (no re-parse per token)", async () => {
    render(<StreamingList />);
    // A long code-heavy stream — many more tokens than the four above.
    const tokens = Array.from({ length: 40 }, (_, i) => `token ${i} `);
    for (const token of tokens) {
      await act(async () => {
        pushToken(token);
      });
    }
    await act(async () => {});

    // Each earlier reply was still parsed exactly once across all 40 tokens.
    expect(earlierRenders()).toEqual([1, 1]);
    expect(md.counts.get(tokens.join(""))).toBeGreaterThanOrEqual(1);
  });
});
