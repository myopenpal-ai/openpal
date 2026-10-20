import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import type { InjectionNotice } from "../lib/types";

type InjectionT = TFunction<"injection">;

/** How long a name may get inside a one-line notice before it's cut. The
 * review card shows the full label instead. */
const SHORT_NAME_MAX_CHARS = 60;

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

function truncate(text: string, max: number): string {
  // By code point, so a cut never splits an emoji or other astral character.
  const chars = Array.from(text);
  return chars.length <= max ? text : `${chars.slice(0, max - 1).join("")}…`;
}

/** A flagged text's label, shortened to fit a one-line notice or status: a
 * page's host, a file's base name, anything else just capped. */
export function injectionShortName(source: string, name: string): string {
  switch (source) {
    case "webPage":
      return truncate(hostOf(name), SHORT_NAME_MAX_CHARS);
    case "file":
      return truncate(name.split(/[/\\]/).pop() || name, SHORT_NAME_MAX_CHARS);
    default:
      return truncate(name, SHORT_NAME_MAX_CHARS);
  }
}

/**
 * The "{{where}}" part of a flagged-text sentence: "the file “x”", "a web
 * page the AI opened", "your message"… `source` is a plain string on the
 * wire and in saved conversations, so a value from a newer build falls
 * through to the generic `where.unknown` phrase rather than a raw key.
 *
 * `toolName` adds the tool's label in parentheses (`where.named`) for the
 * sources whose phrase doesn't already carry a name — the notice line does,
 * the review card doesn't, since it shows the full label right beneath.
 */
export function injectionWhere(
  t: InjectionT,
  source: string,
  name: string | null | undefined,
  { toolName }: { toolName: boolean },
): string {
  let where: string;
  switch (source) {
    case "attachment":
      return name ? t("where.attachment", { name }) : t("where.unknown");
    case "skill":
      return name ? t("where.skill", { name }) : t("where.unknown");
    case "composer":
      return t("where.composer");
    case "projectConversation":
      return t("where.projectConversation");
    case "webPage":
      where = t("where.webPage");
      break;
    case "webSearch":
      where = t("where.webSearch");
      break;
    case "file":
      where = t("where.file");
      break;
    case "commandOutput":
      where = t("where.commandOutput");
      break;
    case "toolOutput":
      where = t("where.toolOutput");
      break;
    default:
      return t("where.unknown");
  }
  return toolName && name ? t("where.named", { where, name }) : where;
}

/** The outcome sentence, or null for an outcome string this build doesn't
 * know — the "found" sentence before it still reads on its own. `earlier`:
 * the text came from a message before the one this reply answers, which the
 * AI had already read. */
function outcomeText(t: InjectionT, outcome: string, earlier: boolean): string | null {
  switch (outcome) {
    case "continued":
      return t("outcome.continued");
    case "stopped":
      return earlier ? t("outcome.stoppedEarlier") : t("outcome.stopped");
    case "sent":
      return t("outcome.sent");
    default:
      return null;
  }
}

/** One notice as a single plain sentence pair: what was found where, then
 * what happened to it. Also what ChatView's status region announces.
 * `turnMessageId` is the user message the reply answers, if known. */
export function injectionNoticeText(
  t: InjectionT,
  notice: InjectionNotice,
  turnMessageId?: string,
): string {
  const name = notice.name ? injectionShortName(notice.source, notice.name) : null;
  const where = injectionWhere(t, notice.source, name, { toolName: true });
  const found = notice.unchecked ? t("unchecked", { where }) : t("found", { where });
  const earlier =
    turnMessageId !== undefined &&
    notice.messageId !== undefined &&
    notice.messageId !== turnMessageId;
  const outcome = outcomeText(t, notice.outcome, earlier);
  return outcome ? `${found} ${outcome}` : found;
}

const WarnIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
    <line x1="12" y1="9" x2="12" y2="13" />
    <line x1="12" y1="17" x2="12.01" y2="17" />
  </svg>
);

/**
 * The prompt-injection notices on one reply: a line per flagged text, saying
 * where it was found and what happened to it (continued, stopped, or sent as
 * typed). Persisted on the message, so they stay after the turn. Names are
 * attacker-influenceable (a URL, a file name) and are rendered as plain text
 * children only — never through `Markdown` or `dangerouslySetInnerHTML`.
 * `turnMessageId`, the user message this reply answers, words a stop of an
 * earlier message's text as such.
 */
export function InjectionNotices({
  notices,
  turnMessageId,
}: {
  notices: InjectionNotice[];
  turnMessageId?: string;
}) {
  const { t } = useTranslation("injection");
  return (
    <ul className="injection-notices" aria-label={t("notice.listLabel")}>
      {notices.map((notice, i) => (
        <li key={i} className="injection-notice">
          <WarnIcon />
          <span className="sr-only">{t("notice.srPrefix")} </span>
          <span className="injection-notice-text">{injectionNoticeText(t, notice, turnMessageId)}</span>
        </li>
      ))}
    </ul>
  );
}
