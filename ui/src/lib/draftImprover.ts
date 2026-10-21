/**
 * Pure decision logic for the composer's "Improve my message" action —
 * kept out of `ChatView.tsx`'s `Composer` (a module-private component with
 * no unit-test convention of its own) so the priority-of-blockers rule, the
 * `/skill` split, and the i18n-key mapping are each testable in isolation.
 * Mirrors `crates/core/src/prompt_enhancer.rs`'s `MAX_DRAFT_CHARS` and
 * `KeepReason` — see that module for the backend half of this contract.
 */

import type { KeepReason } from "./types";
import { parseSkillInvocationDisplay } from "../state/store";
import type { Skill } from "./types";

/** Same value as the Rust `prompt_enhancer::MAX_DRAFT_CHARS` const — kept in
 * sync by hand (a mismatch only means the client-side "too long" hint fires
 * a little early or late; the server-side check is authoritative). */
export const MAX_DRAFT_CHARS = 4000;

/** Split a raw composer draft into a `/skill ` prefix (re-attached after
 * improving, untouched by the model) and the text that's actually sent to
 * `api.enhancePrompt`. A recognized `/skill` with nothing after it still
 * splits (`text` is empty) — `improveBlocker` is what turns that into a
 * disabled state. An unrecognized `/token` is not a skill and is returned
 * as plain draft text, same as `store.ts`'s own `matchSkillInvocation`. */
export function splitDraftForImprove(
  draft: string,
  skills: Skill[],
): { prefix: string; text: string; hadSkillPrefix: boolean } {
  const invocation = parseSkillInvocationDisplay(draft, skills);
  if (!invocation) {
    return { prefix: "", text: draft, hadSkillPrefix: false };
  }
  return {
    prefix: `/${invocation.skill.slug} `,
    text: invocation.rest,
    hadSkillPrefix: true,
  };
}

/** Inverse of the `{ prefix, text }` half of `splitDraftForImprove` — the
 * prefix already carries its own trailing space, so this is a plain
 * concatenation. */
export function joinImproved(prefix: string, text: string): string {
  return `${prefix}${text}`;
}

/** Every reason the "Improve my message" item can be disabled/hidden for.
 * Deliberately has no `"off"` member: the item isn't rendered at all when
 * the `writing-tools` plugin is off, so there is nothing for this union to
 * represent in that case — see the Composer's own gating
 * (`writingToolsAvailable`). */
export type ImproveBlocker =
  | "locked"
  | "improving"
  | "empty"
  | "skillOnly"
  | "tooLong"
  | "diskSlow"
  | "starting"
  | "notReady";

export interface ImproveBlockerInput {
  /** Parental controls currently lock sending. */
  locked: boolean;
  /** An improve request from THIS composer is in flight. */
  improving: boolean;
  /** Mirrors the composer's own `ready` (engine running, or cloud mode). */
  ready: boolean;
  /** The local engine is starting up. */
  starting: boolean;
  /** The running model is disk-slow (mmap-streamed) — see
   * `docs/design/PROMPT_ENHANCER.md`'s "Context policy" for why the action is
   * gated off in this regime. */
  diskSlow: boolean;
  /** The `text` half of `splitDraftForImprove`'s result — what would
   * actually be sent, not the raw draft. */
  draftText: string;
  /** Whether a recognized `/skill` was split off `draftText`. */
  hadSkillPrefix: boolean;
  /** True while the draft is a lone `/word` with no space yet (ChatView's
   * own `/^\/(\S*)$/` test on the RAW draft) — independent of whether the
   * skill-suggestion dropdown happens to be showing. */
  slashMatch: boolean;
}

/** First match wins. `null` means the action is usable. */
export function improveBlocker(input: ImproveBlockerInput): ImproveBlocker | null {
  if (input.locked) return "locked";
  if (input.improving) return "improving";
  if (input.draftText.trim() === "" && !input.hadSkillPrefix) return "empty";
  if (input.slashMatch || (input.hadSkillPrefix && input.draftText.trim() === "")) {
    return "skillOnly";
  }
  if (input.draftText.length > MAX_DRAFT_CHARS) return "tooLong";
  if (input.diskSlow) return "diskSlow";
  if (!input.ready && input.starting) return "starting";
  if (!input.ready) return "notReady";
  return null;
}

/** i18n keys the composer's `improve()` status line can show while a
 * request is in flight or has just settled. */
export type ImproveStatusKey = `chatView.composer.improve.${
  | "working"
  | "done"
  | "undone"
  | "noChange"
  | "noAnswer"
  | "failed"
  | "stopped"}`;

/** i18n keys shown as the disabled improve button's `title`. */
export type ImproveBlockerI18nKey =
  | `chatView.composer.improve.${
      | "offImproving"
      | "offEmpty"
      | "offSkillOnly"
      | "offTooLong"
      | "offDiskSlow"
      | "offStarting"
      | "offNotReady"}`
  | "chatView.parentalLock.placeholder";

export function improveBlockerKey(b: ImproveBlocker): ImproveBlockerI18nKey {
  switch (b) {
    case "locked":
      return "chatView.parentalLock.placeholder";
    case "improving":
      return "chatView.composer.improve.offImproving";
    case "empty":
      return "chatView.composer.improve.offEmpty";
    case "skillOnly":
      return "chatView.composer.improve.offSkillOnly";
    case "tooLong":
      return "chatView.composer.improve.offTooLong";
    case "diskSlow":
      return "chatView.composer.improve.offDiskSlow";
    case "starting":
      return "chatView.composer.improve.offStarting";
    case "notReady":
      return "chatView.composer.improve.offNotReady";
  }
}

/** Maps every `KeepReason` the backend can return onto a status line.
 * Deliberately many-to-one: the seven quality-guard reasons (`empty`,
 * `tooLong`, `tooShort`, `refused`, `answered`, `languageChanged`,
 * `startsWithSlash`) all collapse to the same generic "couldn't improve it"
 * copy — a beginner doesn't benefit from seven distinct failure messages,
 * and per-reason copy would leak model-internals language. */
export function keptStatus(reason: KeepReason): { key: ImproveStatusKey; alert: boolean } {
  switch (reason) {
    case "noChange":
      return { key: "chatView.composer.improve.noChange", alert: false };
    case "noAnswer":
      return { key: "chatView.composer.improve.noAnswer", alert: true };
    case "cancelled":
      return { key: "chatView.composer.improve.stopped", alert: false };
    default:
      return { key: "chatView.composer.improve.failed", alert: true };
  }
}
