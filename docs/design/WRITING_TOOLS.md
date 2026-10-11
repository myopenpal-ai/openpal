# Writing Tools — translate & correct, plugin-gated

Plan produced 2026-08-28 (codebase research fork + manual verification of every
cited file:line before writing this down).

> **2026-09 update:** the shared one-shot helper this plan built,
> `run_writing_tool_stream`, was generalized and renamed to
> `run_one_shot_completion` when the composer's Message improver
> (`docs/design/PROMPT_ENHANCER.md`) reused it — same behaviour for Translate/
> Correct, just a name that no longer implies "Writing Tools only." Every
> `run_writing_tool_stream` reference below is historical.

## Verdict

**Feasible with no new subprocess, no second model, and mostly-existing
plumbing.** Translation and grammar/style correction are both one-shot,
non-conversational completions against the model that's *already* running —
exactly the shape `EngineManager::classify` (`crates/core/src/engine.rs:647`)
already proves works against the live engine with zero extra infrastructure.
The two new pieces of real work are (1) a streaming variant of that one-shot
pattern (classify is non-streaming; a paragraph-length translation shouldn't
make the user stare at a blank pane until the whole thing lands) and (2) a new
UI surface, since neither DeepL nor Scribens is a chat — they're paste-in,
get-result tools.

Per three decisions confirmed with the user up front (see "Decisions locked
in" below), this ships as an **opt-in capability-flag plugin** — same shape as
`hf-browser`/`model-router` (`plugins/PLUGINS.md` §8) — revealing **one
dedicated, non-chat screen** with a Translate/Correct mode toggle, not two
screens and not a chat-composer toggle.

## Decisions locked in (asked and answered before this plan was written)

1. **Placement: opt-in capability-flag plugin**, not an always-visible core
   nav item. Mirrors `hf-browser` exactly — invisible to beginners until they
   open Add-ons, zero default-path complexity added, consistent with
   CLAUDE.md's "gate advanced capability behind Settings → Advanced or the
   plugin system."
2. **UI shape: a dedicated tool screen**, not a chat-composer mode toggle
   (rejected the web-search-globe pattern). Translation and correction are
   single-turn, structured, and *not* a conversation — treating them as a chat
   mode would put transient tool output in Library/conversation history for no
   reason and tempt free-form chat formatting into what should be a clean
   input → output transform.
3. **Scope: both, one tool, two modes.** One plugin (`writing-tools`), one
   screen, a Translate/Correct segmented toggle at the top — not two separate
   plugins or nav entries. They share essentially the entire pipeline
   (one-shot streamed completion against the running engine, fixed
   low-temperature sampling, no conversation persistence); splitting them
   would just duplicate plumbing for no user benefit.

## Verified technical foundation

Every claim below was checked against this worktree's actual source (not
assumed from the fork's summary). File:line citations refer to this branch;
see the engine-refactor note below for what's different on `main`.

1. ✅ **The one-shot-completion-against-the-running-engine pattern already
   exists and is proven**: `EngineManager::classify`/`classify_risk`
   (`crates/core/src/engine.rs:647,671`) both call a shared
   `raw_classify(body)` (`engine.rs:585`) that POSTs a single
   `/v1/chat/completions` request to the already-running llama-server —
   no second process, no second model. `build_classify_body`/
   `build_risk_classify_body` (`engine.rs:843,874`) are pure, unit-tested
   functions that just build the JSON body (system prompt + user text +
   `temperature: 0.0` + a GBNF `grammar` constraining output to one label)
   — the template to mirror for `build_translate_body`/`build_correct_body`.
2. ⚠️ **`raw_classify` is the wrong call to build on directly** — it silently
   returns `Ok(None)` whenever `inner.generating` is true (`engine.rs:588`),
   because llama-server has a single inference slot and classify is a
   best-effort background probe that must never contend with a real reply.
   Translate/Correct is a *foreground, user-initiated* action — if the engine
   is mid-reply to a chat in another view, the new commands must show "the
   engine is busy" and either wait or let the user cancel, never silently
   fail like triage does. This is new behavior, not a reuse of `raw_classify`
   as-is.
3. ✅ **Streaming already exists as `ChatEvent`** (`crates/core/src/types.rs:1094`):
   `Token { text }`, `Done { stats }`, `Error { message }` are generic enough
   to reuse verbatim for a non-chat completion stream — no new event type
   needed.
4. ✅ **`chat_stream`** (`crates/app/src/commands.rs:1613`) is conversation-
   shaped by construction: it takes a `conversation_id`, calls
   `parental::enforce_send(&state)` (`crates/app/src/parental.rs:54`) first,
   then branches on `settings.chat_backend` — `ChatBackend::Cloud` routes to
   `cloud_chat_turn`, agent mode to `agent_chat_turn`, otherwise the plain
   local engine. **Two things worth deliberately deciding, not defaulting
   past:** (a) should Translate/Correct also respect `parental::enforce_send`
   — almost certainly yes, since it's a way to get the model to produce text
   a parental filter would otherwise block; (b) should it also honor
   `settings.chat_backend` (local vs. the newer cloud backend) rather than
   hardcoding local — yes, for consistency with however the user has already
   configured chat, and because "mono-model first" is about not loading a
   *second* model, not about refusing to follow the user's own backend
   choice.
5. ⚠️ **`Settings.system_prompt` must NOT be included.** It's injected into
   ordinary chat turns at four call sites in `commands.rs` (base personality,
   e.g. lines 1760, 1871, 1969, 2013). A user's custom persona
   ("always answer as a pirate") leaking into a translation request would
   silently corrupt the output. The new commands need their own fixed system
   prompt per mode, never `settings.system_prompt`.
6. ✅ **Sampling precedent points at a specific number, not a guess.**
   `general_sampling_defaults` (`crates/core/src/types.rs:794`) has a comment
   directly on point: `TaskType::Writing`'s defaults
   (`temperature: 0.7, top_p: 0.9, top_k: 40`, `types.rs:809`) deliberately
   sit between `Coding` and prose because that bucket "now also covers what
   used to be 'translating' (wants low temperature/precision) alongside prose
   (wants more variety)" (`types.rs:805-808`) — i.e. this repo has *already*
   decided, in writing, that translation wants `Coding`-like determinism
   (`temperature: 0.2, top_p: 0.9, top_k: 20`, `types.rs:799-804`), not
   `Writing`'s creative-variance defaults. Translate/Correct should use a
   fixed preset close to `Coding`'s numbers, hardcoded per mode (not exposed
   as tunable sliders — a beginner shouldn't need to know what temperature
   is), and it should bypass the Generation panel entirely.
7. ✅ **`TaskType` needs no changes.** It briefly had a `translating` tag,
   deliberately folded into `writing` in commit `d4369e6` — but that fold was
   about shrinking the *ambient, silent chat-classification* taxonomy
   (`build_classify_body`'s prompt at `engine.rs:855` literally says "writing
   (including translation)"). A purpose-built tool screen with an explicit
   mode toggle is a different concern entirely and doesn't reopen that
   enum — no reason to add a 6th `TaskType` variant for this.
8. ✅ **The capability-flag plugin pattern is exactly `hf-browser`'s**
   (`plugins/PLUGINS.md` §8, `plugins/hf-browser/plugin.json`): a manifest
   with no `python` block (`{"id", "name", "description", "version", "author",
   "capabilities": [...]}`), `is_set_up()` trivially `true`, `service` always
   `"notApplicable"`. The gate is UI-only —
   `plugins.some(p => p.capabilities.includes("writing-tools") && p.enabled)`
   (mirrors `modelBrowserAvailable`, `ui/src/state/store.ts:450-451`). HF
   Browser's own design decision #2 applies here too: **gating in the new
   Rust commands themselves buys nothing** (they'd be the only
   plugin-checked commands in the app for no security benefit) — the plugin
   gates the *sidebar entry*, not the command.
9. ✅ **Sidebar nav precedent to clone exactly**: the "more" menu's
   `browseAvailable && (...)` block (`ui/src/views/Sidebar.tsx:315-327`),
   fed by `modelBrowserAvailable(plugins)` (`store.ts:450-451`, wired at
   `Sidebar.tsx:135`). `View` (`store.ts:70`) is currently
   `"chat" | "library" | "plugins" | "discover" | "skills"` — needs one more
   member. When the capability disappears mid-session (user disables the
   plugin while the tool is open), redirect to `"chat"`, mirroring the
   `discover` → `"library"` fallback (`store.ts:895-922`).
10. ✅ **Real i18n exists and must be updated**: `ui/src/i18n/locales/en.json`
    and `fr.json` (react-i18next, `useTranslation()` throughout `Sidebar.tsx`)
    — every new UI string needs both locale files touched, not just English.
11. ✅ **A two-pane layout precedent exists to reuse**, not invent:
    `ChatView.tsx`'s `.chat-layout` flex row + `PreviewContext`/
    `DocPreviewPanel` (`ChatView.tsx:567,1241-1497`) shows the established
    pattern for a side-by-side panel. The new screen is simpler (no shared
    context needed, just local component state for source/result panes) but
    should reuse the same CSS shape rather than inventing new layout
    primitives.
12. ⚠️ **The engine layer is mid-refactor on `main`, ahead of this branch.**
    This worktree's branch is 3 commits behind `main` (`b4ab5cc`, `02fde75`,
    `0aa2b83` — introducing an `EngineBackend` trait + `SubprocessBackend` +
    a thin `EngineManager`; being done in a concurrent session on this
    machine, "extract-engine-backend-trait"). Whoever implements this feature
    should rebase onto that shape first — the streaming one-shot helper
    described below belongs on the post-refactor `EngineManager`/
    `EngineBackend`, not this branch's pre-refactor `engine.rs`.

## Design

### UI

One new sidebar entry, "Writing Tools" (icon TBD), gated exactly like
"Browse" — appears in the "more" menu only when the plugin is enabled. One
new view:

```
┌───────────────────────────────────────────────────┐
│  [ Translate ]  [ Correct ]          (segmented)   │
├────────────────────────┬───────────────────────────┤
│  Source text            │  Result                   │
│  (textarea, paste)      │  (streams in live)        │
│                         │                            │
│  Translate: [Detect ▾] → [French ▾]                │
│  Correct: (no extra controls)                       │
│                         │  [Copy]                    │
└────────────────────────┴───────────────────────────┘
```

- **Translate mode**: source-language dropdown (default "Detect"; "detect" is
  just "don't pin the source language" in the prompt, not a separate
  detection call — one fewer round trip), target-language dropdown, streamed
  output.
- **Correct mode**: single input, output pane is the corrected text (plain
  replacement for v1). Inline tracked-changes/diff highlighting (strikethrough
  removed / underline added, Scribens-style) is real UX value but a
  materially separate piece of frontend work (diffing + rendering) — proposed
  as a v2 follow-up, not a v1 blocker.
- **Ephemeral by design**: no Library entry, no conversation, nothing written
  to `app_db` beyond perhaps "last used target language" as a small local
  setting. Mirrors how `classify` calls never become chat history — this is a
  tool, not a conversation.
- **Busy-engine state**: since (per finding 2 above) this can't silently
  no-op like `classify` does, the screen needs an explicit "engine is busy
  generating a chat reply" state — most likely just disabling the
  Translate/Correct action button with that message, since queuing behind an
  unrelated chat turn is a fine default for a local single-slot engine.
- **Local-first framing in the empty state** ("Nothing you type here leaves
  your device") — consistent with existing beginner-facing copy elsewhere
  (onboarding, `SafetyBanner`).

### Backend

*(This was the original high-level sketch, written before the verified
implementation pass below existed. Superseded in the specifics — the actual
function names ended up `translate_system_prompt`/`CORRECT_SYSTEM_PROMPT`/
`fixed_sampling` rather than `build_translate_body`/`build_correct_body`,
and the streaming helper turned out not to need the post-refactor
`EngineBackend` shape after all. See "Verified implementation detail" →
"Reconciliation" for what's actually current; kept here as the original
reasoning trail, not as instructions to follow.)*

- New `crates/core/src/writing_tools.rs`: pure, unit-testable request-body
  builders mirroring `build_classify_body`'s shape —
  `build_translate_body(text, source_lang: Option<&str>, target_lang: &str)`
  and `build_correct_body(text: &str)`. Each hardcodes its own system prompt
  ("You are a translation engine. Translate the user's text from {source} to
  {target}. Output only the translated text, nothing else." /
  "Fix grammar, spelling, and style. Preserve the original meaning and
  language. Output only the corrected text, nothing else.") and a fixed
  `SamplingParams` close to `TaskType::Coding`'s (`temperature: 0.2,
  top_p: 0.9, top_k: 20`) per finding 6 above — never `settings.system_prompt`,
  never the user's Generation-panel sliders.
- These need **streaming**, unlike `classify`/`raw_classify`. Extract (on top
  of the post-refactor `EngineBackend`/`EngineManager`, per finding 12) a
  shared "one-shot streamed completion" helper that `chat_stream` and the new
  commands both call — same SSE-token-forwarding logic that already exists,
  minus the conversation-persistence step. Respect `settings.chat_backend`
  (local vs. cloud, finding 4b) so the tool behaves consistently with
  whatever backend the user already has chat pointed at.
- Two new Tauri commands, `translate_stream(state, text, source_lang,
  target_lang, on_event: Channel<ChatEvent>)` and `correct_stream(state, text,
  on_event: Channel<ChatEvent>)` (`crates/app/src/commands.rs`, registered in
  `crates/app/src/lib.rs`'s `generate_handler!` next to `chat_stream`), each
  calling `parental::enforce_send` first (finding 4a) and emitting the
  existing `Token`/`Done`/`Error` `ChatEvent` variants — the frontend's
  existing streaming-consumption code needs a new call site, not a new shape.
- Friendly-error validation at the command layer (empty text, missing target
  language for Translate) via the existing `friendly()`/`internal()`
  helpers, `Result<T, String>` throughout — same convention as every other
  command.

### Plugin

- `plugins/writing-tools/plugin.json`: python-less, `capabilities:
  ["writing-tools"]`, no `python` block — identical shape to
  `plugins/hf-browser/plugin.json`.
- `modelBrowserAvailable`'s twin: `writingToolsAvailable(plugins: PluginInfo[])
  => plugins.some(p => p.capabilities.includes("writing-tools") && p.enabled)`
  in `ui/src/state/store.ts`, next to `modelBrowserAvailable`.
- `View` gains `"writing-tools"`; `App.tsx` gains a branch; Sidebar's "more"
  menu gains a `writingToolsAvailable && (...)` block cloned from the
  `browseAvailable` block (`Sidebar.tsx:315-327`); redirect to `"chat"` if the
  capability disappears mid-session while that view is open (clone the
  `discover` → `"library"` fallback at `store.ts:895-922`).

### Language list

A small, curated, hand-picked list (roughly 20-30 languages) rather than
"every language the model has ever seen a token of" — consistent with
"curated catalog over model search" elsewhere in this app, and honest about
what a 4-13B local model can actually translate well. Content-only, easy to
grow later; not a design fork.

### Explicitly out of scope for v1

- **Document upload / whole-file translation** (DeepL does this). Would reuse
  `core/attachments.rs` extraction almost for free, but it's genuinely
  additive scope — propose as an immediate v2, not v1.
- **Inline tracked-changes/diff highlighting** for Correct mode (Scribens'
  signature UX). Real value, separate chunk of frontend work — v2.
- **Model-router integration** ("best installed model for this task") — could
  reuse the existing `task_scores` machinery cheaply later, not needed to
  ship v1.
- **A second, translation-specialized model.** Stays a hard non-goal per
  "mono-model first" (`ARCHITECTURE.md`) — full stop, not just a v1 deferral.

## Verified implementation detail

Produced 2026-08-28 by a 13-agent workflow: six areas of this feature (core
request builders, engine/app streaming plumbing, the Tauri command layer,
plugin manifest + capability gating, frontend UI, and localization/tests/CI)
were each independently researched against the real repository, then each
research result was independently adversarially verified by a second agent
that re-read the same files from scratch rather than trusting the first
agent's citations. A final cross-area critique then diffed all six areas'
*concrete artifacts* (proposed code, function names, wire formats) against
each other — catching several incompatibilities no single area's own review
could see, because each area's local contract looked self-consistent in
isolation.

**Net effect: this bundle could not ship as six independent patches.** Four
real, concrete blockers surfaced where two or three areas independently wrote
mutually incompatible code against the same seam. All four are now resolved
below (in "Reconciliation" after this list) — the code shown later in this
section (Areas A/Backend/D/E) is the *already-reconciled* version, not the
raw six-way output. This subsection is kept as-is because the mismatches
themselves, and why each was resolved the way it was, are useful context for
anyone touching this feature later.

### Blockers found (see "Reconciliation" below for how each was closed)

1. **`crates/core/src/writing_tools.rs`'s public interface is contested by
   three different, incompatible assumptions.** The core-builders area is the
   only one that actually wrote, compiled, and unit-tested this file — its
   real interface is `build_translate_body(text, source_lang: Option<&str>,
   target_lang: &str) -> serde_json::Value` / `build_correct_body(text: &str)
   -> serde_json::Value` (each returning a *complete* request body —
   `messages` + `stream: true` + sampling merged in), plus `language_name(id)
   -> Option<&'static str>` and a `Language`/`LANGUAGES` table. The
   engine-streaming area independently concluded this exact shape is wrong
   for the real call sites (`EngineManager::chat_stream` /
   `openpal_core::cloud::chat_stream` want `messages: &[ChatMessage]` +
   `&SamplingParams` as two separate arguments, not a merged JSON body) and
   recommended Phase 1 instead return something like `(system_prompt:
   String, SamplingParams)` — a recommendation never applied back to the
   actual file. The tauri-commands area assumed a *third*, different
   interface — `writing_tools::translate_system_prompt(source, target) ->
   String`, `correct_system_prompt() -> String`, `translate_sampling() ->
   SamplingParams`, `correct_sampling() -> SamplingParams` — none of which
   exist in the real file (only `language_name` matches). **As drafted, the
   Tauri command bodies below will not compile against the real
   `writing_tools.rs`.** See "Suggested reconciliation" under the backend
   section below.
2. **Two mutually exclusive designs for `translate_stream`/`correct_stream`
   themselves**, with different busy/cancel semantics, are proposed for the
   same file — see the "Backend" section below for both in full. Engine-
   streaming proposes a new `run_writing_tool_stream` that atomically
   claims the shared `state.chat_cancel` slot up front and rejects if busy;
   tauri-commands instead calls `plain_chat`/`cloud_chat_turn` directly with
   no busy-check, relying on an explicitly-unverified assumption that
   llama-server's own single inference slot queues concurrent requests
   safely. Only one can ship. Engine-streaming's own verify pass additionally
   found that *even its own* design only closes the race in one direction —
   the existing `plain_chat`/`cloud_chat_turn`/`agent_chat_turn`/
   `web_search_chat_turn` turn-runners still blindly overwrite the shared
   slot with no check of their own — so neither candidate, as currently
   written, actually delivers full bidirectional busy protection.
3. **`WritingToolsView.tsx`'s `t("writingTools.*")` calls and the proposed
   `en.json`/`fr.json` content use different key shapes for roughly 12 of the
   view's ~20 strings.** The component calls `t("writingTools.modeTranslate")`
   / `modeCorrect`, but the locale JSON defines a nested `writingTools.mode.
   translate` / `mode.correct` instead; it calls `detectLanguage` /
   `chooseLanguage`, the JSON defines `language.detect` /
   `language.targetPlaceholder`; it calls `resultPlaceholder`, the JSON only
   has a differently-purposed `resultEmptyState`; and it calls
   `modeAriaLabel`, `sourceLangAriaLabel`, `targetLangAriaLabel`, `stop`,
   `runTranslate`, `runCorrect`, `localFirstNote` — none of which the
   proposed JSON defines at all. Only `title`/`subtitle`/`sourceLabel`/
   `sourcePlaceholder`/`resultLabel`/`copy`/`copied`/`engineBusy` (8 of 20)
   actually align. **As drafted, most of the screen would render raw
   i18next key strings.** Whoever implements needs to pick one key shape and
   make both files agree — both proposals are given in full below so the
   choice is visible.
4. **The wire value for `source_lang`/`target_lang` is inconsistent across
   three areas in a way that would fail every translate request.** Core
   defines the curated language space as lowercase ISO-639-1 ids (`"fr"`,
   `"es"`) with `language_name(id)` documented as *lenient* — an unrecognized
   id degrades to "use it verbatim," never a hard failure. Tauri-commands
   turns that lenient lookup into a hard validation gate:
   `writing_tools::language_name(&target_lang).ok_or_else(|| "Choose a
   language...")?`, rejecting any `target_lang` not found in the id-keyed
   table. Frontend-ui proposes its own, differently-membered static list of
   plain English display strings (`"French"`, `"Mandarin Chinese"`, …) and
   sends the display name itself, verbatim, as `target_lang`/`source_lang` —
   never an id. None of those display strings match any `Language.id`
   (`"fr"`/`"es"`/`"zh"`, not `"French"`/`"Mandarin Chinese"`), so the
   drafted backend validation would reject every request the drafted
   frontend actually sends. **Pick one wire format** (id, recommended, since
   it's what the backend already implements and tested) and make the
   frontend dropdown send that, not a display string.

Two smaller items resolved themselves rather than needing a human call:

- **Duplicate near-identical edits to the same files** (the plugin-gating and
  frontend-ui areas each independently proposed `store.ts`'s
  `writingToolsAvailable` selector, the `View`-union member, and the
  mid-session redirect; the plugin-gating and i18n-tests-ci areas each
  independently wrote a slightly different `plugins/writing-tools/
  plugin.json`; the plugin-gating and i18n-tests-ci areas each independently
  caught the same required fix to
  `discovers_the_real_repo_plugins_including_hf_browser`). None of these
  disagree in *intent* — they just need one canonical author picked during
  implementation rather than two versions applied. Below, one version of
  each is presented as canonical with a note on where the near-duplicate
  came from.
- **Parental-gate ordering**: both the engine-streaming and tauri-commands
  areas' own self-verify passes independently caught the same real bug —
  calling `parental::enforce_send` (which unconditionally burns one of the
  day's message-quota slots) *before* validating that `text` is non-empty or
  that a language selection is valid means a blank submission or bad pick
  still costs quota for a request that never reaches the model. Both
  converged on the same fix: **validate first, gate second.** This one is
  already resolved — apply it to whichever backend design is chosen.

**Plan-coverage gaps** (things this document's own Phases section calls for
that no area actually produced a concrete task for): a real Phase 0 rebase
task (every area only checked that *its own* files happen to be untouched by
the in-flight engine refactor's diff — none of them schedules the actual
rebase itself as work), and Phase 2's `#[ignore]`d live-engine streaming test
(no area proposed it; folded into Phase 2 below).

**CI status, re-confirmed independently**: still broken on `main`. The
i18n/tests area re-verified this via `gh run view` on `main`'s latest
push-triggered run (`33061223665`, commit `3257d46`, 2026-08-27) — the
`frontend` job still fails `npm ci` (EUSAGE, `ui/package-lock.json` out of
sync — missing `@emnapi/core@1.11.3`/`@emnapi/runtime@1.11.3`) and the `rust`
job still fails `cargo fmt --check` on `crates/app/src/commands.rs:19`. Both
are pre-existing and unrelated to this feature (see the earlier turn's CI
investigation on PR #66) but remain a real prerequisite for this feature's
own CI to ever go green.

### Reconciliation — decisions made to close the four blockers

The multi-agent pass deliberately stopped short of picking between
mutually-exclusive designs (that's a judgment call, not something six
parallel research agents blind to each other's output should resolve by
accident). Closing that loop by hand, before this plan is treated as
implementation-ready:

**Blockers #1 and #2 (the `writing_tools.rs` interface, and which
`translate_stream`/`correct_stream` design ships) are resolved together**,
because they're really one decision: **adopt Option 1** (a new
`run_writing_tool_stream` helper, atomic `chat_cancel` check-and-claim,
reject-up-front busy handling) **and change `writing_tools.rs`'s public
interface to match it** — system-prompt strings plus a shared
`SamplingParams` getter, not a merged `serde_json::Value` body. Reasoning:

- Option 1 is the design that actually closes the busy-race the original
  research explicitly called for ("never silently no-op like `raw_classify`
  does") — Option 2 ships a real, known correctness gap (no busy-check at
  all, resting on an explicitly-unverified assumption about llama-server
  queuing concurrent requests safely) in exchange for a smaller diff. A
  known correctness gap is worse than a slightly bigger diff.
- Option 1's own verify pass found it only closes the race in *one*
  direction — the existing `plain_chat`/`cloud_chat_turn`/`agent_chat_turn`/
  `web_search_chat_turn` turn-runners still unconditionally overwrite
  `state.chat_cancel` with no check of their own, so an ordinary chat turn
  started while Translate/Correct is streaming would still clobber the slot.
  **Adopted as part of this decision, not deferred**: extract the
  check-and-claim block Option 1 already has into a small shared helper —
  e.g. `fn try_claim_generation_slot(state: &AppState) -> Result<Arc<AtomicBool>, String>`
  doing the same atomic `{ lock; if is_some() return Err(...); set; }` — and
  make all six call sites (the four existing turn-runners plus the two new
  commands) go through it instead of assigning `state.chat_cancel` directly.
  This repo's actual current `plain_chat`/`cloud_chat_turn`/
  `agent_chat_turn`/`web_search_chat_turn` call sites were not individually
  re-read as part of this reconciliation pass (that's real implementation
  work, not a planning-time citation) — treat "locate and swap all four" as
  an explicit Phase 2 task, not a citation to trust blindly.
- Given Option 1 wins, its call site needs `Vec<ChatMessage>` +
  `&SamplingParams`, not a merged JSON body — so Area A's file changes
  from `build_translate_body`/`build_correct_body` (returning
  `serde_json::Value`) to `translate_system_prompt`/`CORRECT_SYSTEM_PROMPT`
  (returning a `String`/`&'static str`) plus a now-`pub` `fixed_sampling()`.
  This is a strict simplification, not just a compatibility fix: it also
  deletes the float-precision-through-JSON footgun Area A's own tests had
  to work around (`0.2_f32 as f64` vs. the `f64` literal `0.2`) — comparing
  `SamplingParams` fields directly, as plain `f32`s, needs no such care.
  The corrected file and tests are shown in Area A below; Option 2's full
  code is kept only as a rejected alternative under "Backend" for the
  reasoning trail, not as something to implement.

**Blocker #3 (i18n key shape) is resolved as flat keys** — matching this
app's existing convention (`en.json`'s `sidebar.*` block is flat, not
nested: `newChat`, `conversationsAriaLabel`, not `chat.new`/`conversation.
ariaLabel`) and, more directly, matching every key
`WritingToolsView.tsx`'s code actually calls. The nested shape is dropped
entirely rather than kept as an alternative — there's no reason to prefer it
once it's clear it was never what the component consumed. The corrected,
flat `en.json`/`fr.json` block, trimmed to exactly the ~20 keys the
component calls (dropping `working.*`/`genericError`/`smallModelHint` from
the original draft, since the component never calls them — adding unused
i18n keys is dead weight the same way the already-dropped
`emptyTextError`/`missingTargetLanguageError` were), is shown under Area E
below.

**Blocker #4 (language wire format) is resolved in favor of ids**, per the
original recommendation — it's what the backend already implements and
tests. `ui/src/lib/writingToolsLanguages.ts` changes from a flat array of
English display strings to an array of `{ id, name }` pairs mirroring core's
`Language`/`LANGUAGES` exactly (same ids, same names, same order); the
dropdowns render `name` but send `id` as the `<option value>`. The
duplicate-list drift risk this creates (one list in Rust, one in TypeScript,
kept in sync by hand) is real and stays an accepted, explicitly-flagged v1
tradeoff — see "Open questions" below — rather than something this
reconciliation pass solves by inventing a new Tauri command to serve the
list from Rust; that's a reasonable future polish item, not a blocker.

### A. Core request builders, sampling, and language list

**Status: holds up, with the interface rewritten per "Reconciliation"
above.** The research agent's original file (returning merged
`serde_json::Value` bodies via `SamplingParams::merge_into`) was
independently compiled, unit-tested (9/9 passing), and clippy-checked in an
isolated scratch crate against this repo's pinned `serde`/`serde_json`
versions — real signal that the *approach* (a pure, dependency-free module
mirroring `build_classify_body`'s style) is sound. But that exact interface
turned out to be the wrong shape for either backend design (blocker #1), so
the version below swaps `build_translate_body`/`build_correct_body`
(`-> serde_json::Value`) for `translate_system_prompt`/
`CORRECT_SYSTEM_PROMPT` (`-> String` / `&'static str`) plus a `pub
fixed_sampling() -> SamplingParams` — the command layer now builds its own
`Vec<ChatMessage>` and passes `&SamplingParams` straight through, matching
`chat_stream`'s own call convention exactly. This was not independently
recompiled after the rewrite (that's real implementation work), but it's a
strict simplification of already-verified logic: same language table, same
prompt wording, same fixed sampling values, less code (no JSON
construction, no `merge_into`, no float-through-JSON round trip to guard
against in tests).

No `grammar` field is used anywhere (output is free text, not a closed label
set, so a GBNF single-token constraint doesn't apply here the way it does in
`build_classify_body`). The temperature/top_p/top_k = 0.2/0.9/20 preset is
hand-redefined in `fixed_sampling()` rather than calling
`general_sampling_defaults(TaskType::Coding)` directly, so a future retune of
chat's own Coding defaults can't silently retune this tool too. One shared
preset backs both modes today (a future split would need a mode parameter).
`language_name`/`Language`/`LANGUAGES`/`fixed_sampling` are all `pub` — the
command layer (Area "Backend" below) calls every one of them.

New file, `crates/core/src/writing_tools.rs`:

```rust
//! Pure request-body builders for the Writing Tools plugin's Translate and
//! Correct commands (`writing-tools` capability, `plugins/writing-tools`) —
//! the translate/correct twin of `crate::engine`'s `build_classify_body`/
//! `build_risk_classify_body`: pure functions, no I/O, unit-testable without
//! a live server. Unlike those, these back a foreground, user-initiated,
//! *streamed* free-text completion rather than classify's background,
//! non-streaming, single-label probe.
//!
//! Deliberately has no dependency on `crate::settings` at all — in
//! particular, the user's chat-persona system prompt (a free-text `Settings`
//! field) must never leak into a translate/correct request. Each mode below
//! hardcodes its own fixed system prompt from scratch and its own fixed,
//! non-tunable [`SamplingParams`] preset close to `TaskType::Coding`'s
//! (temperature 0.2 / top_p 0.9 / top_k 20 — `general_sampling_defaults` in
//! `crate::types` already notes, in its own doc comment, that this is the
//! right determinism target for translation-like output). That preset is
//! redefined locally rather than reused via
//! `general_sampling_defaults(TaskType::Coding)`, so a future retune of
//! chat's own Coding defaults can't silently drag this tool's output along
//! with it.
//!
//! Unlike `build_classify_body`/`build_risk_classify_body`, no `grammar`
//! field is used here: those constrain output to exactly one label from a
//! closed set, which is exactly what a GBNF rule is for; translate/correct
//! produce free-form text of unknown length, which a single-choice grammar
//! can't express.
//!
//! Returns plain system-prompt strings and a `SamplingParams` value, not a
//! merged request body — `EngineManager::chat_stream` and
//! `openpal_core::cloud::chat_stream` both take `messages: &[ChatMessage]`
//! plus `&SamplingParams` as separate arguments, so the command layer
//! (`crates/app/src/commands.rs`) assembles the actual `Vec<ChatMessage>`
//! itself and passes this module's `SamplingParams` straight through.

use crate::types::SamplingParams;

/// One entry in the curated language list Translate mode's dropdowns are
/// built from. A small, hand-picked catalog (roughly 20-30 languages), not
/// "every language the base model has ever seen a token of" — the same
/// curated-over-exhaustive posture as `crate::catalog`, and honest about
/// what a 4-13B local model actually translates well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Language {
    /// Stable wire id (lowercase ISO 639-1 where one exists) — what the
    /// frontend's dropdowns send as `source_lang`/`target_lang`.
    pub id: &'static str,
    /// English display name, substituted into the translate system prompt
    /// and shown in the dropdown.
    pub name: &'static str,
}

/// The curated language list. Content-only; growing it later is not a
/// structural change to this module — see "Open questions" in
/// `docs/design/WRITING_TOOLS.md` on how membership should eventually be decided.
pub const LANGUAGES: &[Language] = &[
    Language { id: "en", name: "English" },
    Language { id: "es", name: "Spanish" },
    Language { id: "fr", name: "French" },
    Language { id: "de", name: "German" },
    Language { id: "it", name: "Italian" },
    Language { id: "pt", name: "Portuguese" },
    Language { id: "nl", name: "Dutch" },
    Language { id: "sv", name: "Swedish" },
    Language { id: "no", name: "Norwegian" },
    Language { id: "da", name: "Danish" },
    Language { id: "fi", name: "Finnish" },
    Language { id: "pl", name: "Polish" },
    Language { id: "cs", name: "Czech" },
    Language { id: "ro", name: "Romanian" },
    Language { id: "hu", name: "Hungarian" },
    Language { id: "el", name: "Greek" },
    Language { id: "ru", name: "Russian" },
    Language { id: "uk", name: "Ukrainian" },
    Language { id: "tr", name: "Turkish" },
    Language { id: "ar", name: "Arabic" },
    Language { id: "he", name: "Hebrew" },
    Language { id: "hi", name: "Hindi" },
    Language { id: "vi", name: "Vietnamese" },
    Language { id: "th", name: "Thai" },
    Language { id: "id", name: "Indonesian" },
    Language { id: "zh", name: "Chinese (Simplified)" },
    Language { id: "ja", name: "Japanese" },
    Language { id: "ko", name: "Korean" },
];
// NOTE: `cargo fmt` will expand each `Language { .. }` literal above to one
// field per line (this repo has no rustfmt.toml override, and the default
// struct_lit_width is exceeded) — verified by actually running rustfmt on
// this file. Run `cargo fmt -p openpal-core` after creating this file and
// commit the expanded form; don't hand-collapse it back, or CI's
// `cargo fmt --check` (the very first gating step) will fail.

/// Look up a curated language's display name by its wire id. `None` when
/// `id` isn't in [`LANGUAGES`] — callers treat that as "use the raw id
/// verbatim" rather than a hard failure, so a stale id from an older
/// frontend build degrades gracefully instead of erroring.
pub fn language_name(id: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|l| l.id == id).map(|l| l.name)
}

/// Fixed sampling preset shared by both Translate and Correct: close to
/// `TaskType::Coding`'s determinism (temperature 0.2 / top_p 0.9 / top_k 20),
/// never exposed to the user as tunable sliders — Writing Tools bypasses the
/// Generation panel entirely. `pub`: the command layer passes this straight
/// to `EngineManager::chat_stream`/`cloud::chat_stream` as `&SamplingParams`.
pub fn fixed_sampling() -> SamplingParams {
    SamplingParams {
        temperature: 0.2,
        top_p: 0.9,
        top_k: 20,
        ..SamplingParams::default()
    }
}

/// Build the fixed system prompt for a streamed translate completion. Pure
/// (no I/O), unit-testable without a live server, and returns the prompt
/// text only — the command layer wraps it into a `ChatMessage` itself,
/// alongside the user's text and [`fixed_sampling`].
///
/// `target_lang` and `source_lang` are wire ids from [`LANGUAGES`] (e.g.
/// `"fr"`); `source_lang: None` means "detect" — the prompt simply omits the
/// source-language clause instead of a separate detection round trip.
pub fn translate_system_prompt(source_lang: Option<&str>, target_lang: &str) -> String {
    let target = language_name(target_lang).unwrap_or(target_lang);
    match source_lang.map(|id| language_name(id).unwrap_or(id)) {
        Some(source) => format!(
            "You are a translation engine embedded in a writing tool. \
             Translate the user's text from {source} into {target}. \
             Preserve the original meaning, tone, and formatting (line \
             breaks, lists, punctuation). Output only the translated text \
             in {target} — no explanations, no notes, no quotation marks, \
             and do not restate the original."
        ),
        None => format!(
            "You are a translation engine embedded in a writing tool. \
             Translate the user's text into {target}. Preserve the \
             original meaning, tone, and formatting (line breaks, lists, \
             punctuation). Output only the translated text in {target} — \
             no explanations, no notes, no quotation marks, and do not \
             restate the original."
        ),
    }
}

/// Fixed system prompt for Correct mode. A `const` so its exact wording is a
/// single, greppable, unit-tested source of truth.
pub const CORRECT_SYSTEM_PROMPT: &str = "You are a grammar and style correction \
    engine embedded in a writing tool. Fix grammar, spelling, and \
    punctuation mistakes in the user's text, and improve awkward phrasing, \
    without changing its meaning or the language it is written in. Output \
    only the corrected text — no explanations, no notes, no quotation \
    marks, and do not comment on what you changed.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_prompt_names_source_and_target_languages() {
        let system = translate_system_prompt(Some("en"), "fr");
        assert!(system.contains("English"), "should name the source language: {system}");
        assert!(system.contains("French"), "should name the target language: {system}");
    }

    #[test]
    fn translate_prompt_omits_source_clause_when_detecting() {
        let system = translate_system_prompt(None, "fr");
        assert!(!system.contains(" from "), "detect mode should not name a source language: {system}");
        assert!(system.contains("French"));
    }

    #[test]
    fn translate_prompt_falls_back_to_the_raw_id_for_an_unknown_language() {
        let system = translate_system_prompt(None, "xx-unknown");
        assert!(system.contains("xx-unknown"));
    }

    #[test]
    fn correct_prompt_preserves_meaning_and_language() {
        assert!(
            CORRECT_SYSTEM_PROMPT.contains("without changing"),
            "must instruct the model to preserve meaning/language"
        );
    }

    #[test]
    fn fixed_sampling_matches_codings_low_temperature_determinism() {
        let s = fixed_sampling();
        assert_eq!(s.temperature, 0.2);
        assert_eq!(s.top_p, 0.9);
        assert_eq!(s.top_k, 20);
    }

    #[test]
    fn every_curated_language_has_a_non_empty_id_and_name() {
        for lang in LANGUAGES {
            assert!(!lang.id.is_empty());
            assert!(!lang.name.is_empty());
        }
    }

    #[test]
    fn language_ids_are_unique() {
        let mut ids: Vec<&str> = LANGUAGES.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LANGUAGES.len(), "duplicate language id in LANGUAGES");
    }

    /// Guards the risk register's top concern for this module: a translate
    /// or correct request must never carry the user's chat-persona system
    /// prompt. This module never imports `crate::settings` in the first
    /// place, so there is no value in scope to leak — this test enforces
    /// that structurally, by scanning this file's own source for the
    /// identifier. The needle is built at runtime (not written as one
    /// literal) so this assertion doesn't trip on itself.
    #[test]
    fn this_module_never_references_the_user_chat_persona_field() {
        let forbidden = format!("{}{}", "system_", "prompt");
        let source = include_str!("writing_tools.rs");
        assert!(
            !source.contains(&forbidden),
            "writing_tools.rs must never reference Settings' free-text persona field"
        );
    }
}
```

(No `grammar`/`stream`/`messages` assertions remain — those were properties
of the old JSON-body interface. The command layer, not this module, now owns
building the actual `Vec<ChatMessage>` and setting `stream: true` on the
outgoing request, the same way `chat_stream` already does for ordinary chat.)

`crates/core/src/lib.rs` gets two small, independent insertions: a
module-doc bullet next to the `engine_update` bullet (placement is a
judgment call — the list is topical, not alphabetical; either at the end or
next to `engine`/`cloud` is defensible, just don't skip it), and
`pub mod writing_tools;` as the new last line of the `pub mod` block, which
is strictly alphabetical (`writing_tools` sorts after `update_portable`).

Risks specific to this area: the self-referential `include_str!` test is
fragile in one specific way (writing the literal substring `system_prompt`
anywhere else in this file's own prose would make the test fail on itself);
rustfmt will reformat `LANGUAGES` into multi-line form, so don't hand-collapse
it back. Open questions: exact system-prompt wording deserves a
prompt-engineering pass against actual small local models before shipping;
the 28-language membership is a proposal, not a decision, per this doc's own
"Open questions" below; whether `fixed_sampling()` should ever diverge
between the two modes.

### Backend: streaming plumbing + `translate_stream`/`correct_stream` — adopted design

**Adopted: a new `run_writing_tool_stream` helper with an atomic
check-and-claim busy gate (the engine-streaming area's design, "Option 1"),
hardened per its own verify finding, and now written against the corrected
`writing_tools.rs` interface from Area A above.** See "Reconciliation" above
for why this was chosen over the tauri-commands area's alternative (direct
`plain_chat`/`cloud_chat_turn` reuse with no busy-check) — that alternative
is kept below as a rejected option, briefly, for the reasoning trail.

Both research areas agree on one fact, verified independently against both this
branch and local `main`'s post-refactor tip: `git diff HEAD main -- crates/app/`
is **empty** — the in-flight `EngineBackend` refactor (`b4ab5cc`/`02fde75`/
`0aa2b83`, on local `main` but not yet pushed to `origin/main`) touches
nothing under `crates/app/`, and `EngineManager::chat_stream`'s signature is
byte-for-byte identical pre- and post-refactor:

```rust
pub async fn chat_stream(
    &self,
    messages: &[ChatMessage],
    sampling: &SamplingParams,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(ChatEvent) + Send,
) -> Result<()>
```

So whichever design below is chosen, it's valid whether written before or
after Phase 0's rebase — the earlier finding 12 in this document (extract
the helper "on top of the post-refactor EngineBackend") turns out to be
unnecessary: `chat_stream` already *is* the generic one-shot streamed
primitive, unchanged by the refactor.

#### Adopted design — `run_writing_tool_stream`, reject-up-front busy handling, hardened bidirectionally

Recommendation: the shared helper lives in `crates/app/src/commands.rs` (a
new private fn), not in `crates/core`'s engine layer — it needs `AppState`
(settings, account, `chat_cancel`, http, engine), which core deliberately
doesn't have access to.

Busy behavior: **reject up front** with a friendly error, never queue or
wait, and never silently no-op like `raw_classify`. `state.chat_cancel`
(`crates/app/src/state.rs:401`) is a single global `Option<Arc<AtomicBool>>`
slot shared by every turn-runner (`plain_chat`/`cloud_chat_turn`/
`agent_chat_turn`/`web_search_chat_turn`); two concurrent claims would let
one turn's cleanup silently orphan the other's Stop button and idle-timer
bookkeeping. The busy check reads `state.chat_cancel.lock().await.is_some()`
— **not** `state.engine.activity().await.map(|a| a.generating)`, because
`begin_generation`/`end_generation` (which set that flag) are called only by
`plain_chat`/`agent_chat_turn`/`web_search_chat_turn`, never by
`cloud_chat_turn` — so `generating` is blind to an in-flight cloud turn.
Check-and-claim happens atomically inside one `.lock().await` scope (read
`is_some()` and, if free, immediately set the slot, all before releasing the
guard and before any `.await` on the network) to close a TOCTOU race.
Cancel: reuse the existing `stop_generation` command as-is — no new cancel
command needed, since bidirectional check-and-claim (below) guarantees at
most one of {chat, translate/correct} ever occupies the slot at a time.

**Hardening applied per this design's own verify finding (blocker #2's
residual gap, now closed):** the check-and-claim block below is factored out
into a shared helper so the four *existing* turn-runners use it too, instead
of each unconditionally assigning `state.chat_cancel` the way they do today.
Without this, the busy race is only closed in the translate/correct → chat
direction; an ordinary chat turn started while a translation is streaming
would still silently clobber the slot.

```rust
// crates/app/src/commands.rs — factored out of what was previously
// run_writing_tool_stream's own inline block, so plain_chat/cloud_chat_turn/
// agent_chat_turn/web_search_chat_turn can all share it. Locating and
// updating those four existing call sites (today each presumably does
// something like `*state.chat_cancel.lock().await = Some(cancel.clone());`
// unconditionally) is real Phase 2 implementation work — the exact current
// code at each site was not re-verified as part of this planning pass.

/// Atomically claim the app's single foreground-generation slot, or return a
/// friendly busy error if something else already holds it. Every turn-runner
/// (chat, agent, web-search, and Writing Tools) must go through this instead
/// of assigning `state.chat_cancel` directly, or the busy-race this exists to
/// close reopens in whichever direction skips it.
async fn try_claim_generation_slot(state: &AppState) -> Result<Arc<AtomicBool>, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut slot = state.chat_cancel.lock().await;
    if slot.is_some() {
        return Err("OpenPal is busy replying to a chat message right now — try again in a moment.".to_string());
    }
    *slot = Some(cancel.clone());
    Ok(cancel)
}
```

**Corrections applied**: (1) from this design's own verify pass — the order
is `parental::enforce_send` → input validation → per-branch fallible setup
(`resolve_cloud_access` for Cloud) → atomic check-and-claim immediately
before the actual generation call; (2) validation must run **before**
`parental::enforce_send`, not after (the tauri-commands area's verify pass
independently caught this same ordering bug on its own draft — burning a
parental-controls quota slot on a request that never reaches the model is
wrong regardless of which design ships); (3) check-and-claim now goes through
the shared `try_claim_generation_slot` helper above instead of an inline
block, so the same protection applies uniformly; (4) built against Area A's
corrected interface (`translate_system_prompt`/`CORRECT_SYSTEM_PROMPT`/
`fixed_sampling`, not `build_translate_body`/`build_correct_body`) — the
command layer now assembles `Vec<ChatMessage>` itself:

```rust
// crates/app/src/commands.rs — new "Writing Tools" section, between
// "Chat" (ends at stop_generation) and "Speech"

/// Pulled out of `cloud_chat_turn`'s entitlement/token block so
/// `run_writing_tool_stream` can reuse it without duplicating the retry
/// logic. No behavior change to `cloud_chat_turn` itself.
async fn resolve_cloud_access(state: &AppState, settings: &Settings) -> Result<(String, String), String> {
    require_entitlement(state, "cloud-models").await?;
    let model_id = settings
        .selected_cloud_model_id
        .clone()
        .ok_or_else(|| "Choose a cloud model in Settings first.".to_string())?;
    let access_token = state.account.access_token().await.or_else(|_| {
        // existing one-retry-via-refresh behavior, unchanged
        Err("Sign in again to use cloud models.".to_string())
    })?;
    Ok((model_id, access_token))
}

/// Shared streamed one-shot-completion path for Translate/Correct. Never
/// references `settings.system_prompt` — callers pass a complete, already-
/// built `messages` array. Rejects up front (no queue) via
/// `try_claim_generation_slot` if the app's single foreground-generation
/// slot is already claimed by a chat turn (or vice versa, once the four
/// existing turn-runners are updated to claim through the same helper).
async fn run_writing_tool_stream(
    state: &AppState,
    settings: &Settings,
    messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    parental::enforce_send(state).await?;

    let cloud_access = if settings.chat_backend == ChatBackend::Cloud {
        Some(resolve_cloud_access(state, settings).await?)
    } else {
        None
    };

    let cancel = try_claim_generation_slot(state).await?;

    let result = if let Some((model_id, access_token)) = cloud_access {
        openpal_core::cloud::chat_stream(&state.http, &access_token, &model_id, &messages, sampling, cancel, |event| {
            let _ = on_event.send(event);
        })
        .await
    } else {
        state.engine.begin_generation().await;
        let r = state
            .engine
            .chat_stream(&messages, sampling, cancel, |event| {
                let _ = on_event.send(event);
            })
            .await;
        state.engine.end_generation().await;
        r
    };

    *state.chat_cancel.lock().await = None;
    result.map_err(friendly)
}

fn system_message(content: String) -> ChatMessage {
    ChatMessage { role: "system".to_string(), content, model: None, citations: None, thinking: None,
        steps: None, attachments: None, artifacts: None, stats: None, display_content: None }
}

fn user_message(content: String) -> ChatMessage {
    ChatMessage { role: "user".to_string(), content, model: None, citations: None, thinking: None,
        steps: None, attachments: None, artifacts: None, stats: None, display_content: None }
}

#[tauri::command]
pub async fn translate_stream(
    state: State<'_, AppState>,
    text: String,
    source_lang: Option<String>,
    target_lang: String,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Type or paste some text to translate.".to_string());
    }
    let source_lang = source_lang.filter(|s| !s.is_empty());
    if let Some(id) = source_lang.as_deref() {
        if writing_tools::language_name(id).is_none() {
            return Err("That source language isn't on OpenPal's list. Leave it on Detect, or pick one from the menu.".to_string());
        }
    }
    if writing_tools::language_name(&target_lang).is_none() {
        return Err("Choose a language to translate into.".to_string());
    }

    let settings = state.settings.lock().await.clone();
    let system = writing_tools::translate_system_prompt(source_lang.as_deref(), &target_lang);
    let messages = vec![system_message(system), user_message(text)];
    run_writing_tool_stream(&state, &settings, messages, &writing_tools::fixed_sampling(), on_event).await
}

#[tauri::command]
pub async fn correct_stream(
    state: State<'_, AppState>,
    text: String,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Type or paste some text to correct.".to_string());
    }

    let settings = state.settings.lock().await.clone();
    let messages = vec![
        system_message(writing_tools::CORRECT_SYSTEM_PROMPT.to_string()),
        user_message(text),
    ];
    run_writing_tool_stream(&state, &settings, messages, &writing_tools::fixed_sampling(), on_event).await
}
```

(`system_message`/`user_message` build the full `ChatMessage` literal this
codebase uses at every other call site — field list taken from the
tauri-commands area's own citation of the real struct; verify it against
`crates/core/src/types.rs`'s actual `ChatMessage` definition during
implementation, since it wasn't independently re-confirmed in this
reconciliation pass.)

`crates/app/src/lib.rs`: add `commands::translate_stream,` and
`commands::correct_stream,` to the `generate_handler!` list next to
`commands::chat_stream, commands::stop_generation,` (both research areas
independently arrived at the same insertion point).

No change needed to `crates/core/src/engine/mod.rs` — confirmed by reading
`main`'s tip in full: `EngineManager::chat_stream` already has the exact
signature needed; the "extract a helper on the post-refactor EngineBackend"
framing in this document's earlier "Verified technical foundation" section
turns out to be unnecessary.

Risks (adopted design): `chat_stream`'s POST has no `.timeout()` at all
(pre-existing, not new); reject-up-front is a firmer UX stance than "queue
quietly" — matches this document's own UI sketch ("disable the button with
that message"); `begin_generation`/`end_generation` idle-tracking is
local-engine-only (mirrors `plain_chat`'s existing behavior), the cloud
branch has none, same as today.

#### Rejected alternative — reuse `plain_chat`/`cloud_chat_turn` directly, no busy-check

The tauri-commands area's independent proposal: skip a new helper entirely
and have the two new commands call the existing private `plain_chat`/
`cloud_chat_turn` turn-runners directly (they already don't touch Library/
conversation history, and inserting `settings.system_prompt` only happens
when `messages` has no system message yet — a no-op once translate/correct
always supply their own). Smaller diff, zero new engine/app plumbing — but
it ships with an explicitly unverified assumption (that llama-server's
single inference slot safely queues a second concurrent request rather than
erroring) **and no busy-check at all**, which is exactly the correctness gap
"Reconciliation" above rejected it for. Not adopted. If a future revision of
this feature wants the smaller diff badly enough to accept that gap, this
option's full code is preserved in this document's git history (the version
of this file committed to PR #66 before this reconciliation pass).

### C. Plugin manifest and capability-flag gating

**Status: two areas (plugin-gating, i18n-tests-ci) each independently wrote
a `plugins/writing-tools/plugin.json` with slightly different copy, and two
areas (plugin-gating, frontend-ui) each independently wrote the same
`store.ts` selector.** One canonical version of each is given below;
implement only one.

`plugins/writing-tools/plugin.json` (canonical version — plugin-gating
area's; i18n-tests-ci's independent draft used near-identical structure with
different `description` wording, superseded by this one):

```json
{
  "id": "writing-tools",
  "name": "Writing Tools",
  "description": "Turns the model you already have loaded into a paste-in translation and grammar/style tool — one screen, Translate and Correct modes, no chat and no second model to download. Turn this on if you want quick translations or writing fixes without starting a conversation.",
  "version": "0.1.0",
  "author": "OpenPal",
  "capabilities": ["writing-tools"]
}
```

**Correction applied** (from this area's own verify pass): the tone-check
citing "254 chars"/"270 chars" for the hf-browser/model-router precedents
was wrong when actually measured (210 and 289 chars respectively) — doesn't
change the qualitative conclusion that the drafted copy (268 chars) is a
comparable length, just the cited numbers. Also flagged: "Writing Tools" as
the exact display name is still an **open, unresolved question** per this
document's own "Open questions" section below — this manifest content is a
draft against that open question, not a locked value.

`plugins/writing-tools/README.md` (new file, mirroring
`plugins/model-router/README.md`'s structure):

```markdown
# Writing Tools (writing-tools) plugin

One-shot Translate/Correct screen that runs on the model you already have
loaded — never a second, translation-specialized model. Paste text in, get a
streamed result out; nothing is saved to Library or conversation history.

## How it fits together

This is an OpenPal plugin (see `../PLUGINS.md`) in name only — it has **no**
`python` block. It exists purely as an opt-in switch: enabling it advertises
the `writing-tools` capability, which reveals the "Writing Tools" sidebar
entry and view. All the actual work lives in the main app —
`crates/core/src/writing_tools.rs`'s request builders (fixed system prompt +
Coding-like low-temperature sampling per mode, never `Settings.
system_prompt`), the `translate_stream`/`correct_stream` Tauri commands
(`crates/app/src/commands.rs`), and `ui/src/views/WritingToolsView.tsx`. See
§8 of `../PLUGINS.md` for why a UI-only capability flag is a legitimate
plugin.

Unlike `model-router`, this capability gates a UI surface only:
`translate_stream`/`correct_stream` are net-new commands with no other call
site, so they are deliberately **not** gated by
`PluginManager::capability_enabled` — same as `hf-browser`'s ungated HF
commands (see `docs/design/HF_BROWSER.md`'s decision 2).

It's a plugin (rather than an always-visible core nav item) so beginners
never see it: OpenPal's default experience is one clean Chat view; Writing
Tools is the opt-in escape hatch for anyone who wants dedicated
translate/correct tooling without it cluttering the default UI.

## Why python-less plugins are excluded from packaged Portable builds

Nothing here needs a host Python or network setup step, but Portable mode
skips *every* built-in plugin uniformly (see `docs/design/PORTABLE.md`) — dropping
this folder into `<data>/plugins/` on a Portable install still works if you
want it there.
```

`ui/src/state/store.ts` (canonical version — plugin-gating area's fuller
doc comment; frontend-ui independently proposed the same function with a
terser comment):

```ts
/** Whether the Writing Tools plugin is enabled — gates the "writing-tools"
 * sidebar entry and view (Translate/Correct). Same capability-and-`enabled`
 * check as `modelBrowserAvailable`, not a fixed plugin id, so any plugin
 * that advertises `writing-tools` lights it up. Deliberately does NOT check
 * `service` (a python-less plugin's service is always "notApplicable", never
 * "running" — see plugins/PLUGINS.md §8) or `setup` (trivially "ready" the
 * instant a python-less plugin is enabled). Like `modelBrowserAvailable`
 * (and unlike `modelRouterAvailable`), this gates a navigation destination,
 * so it needs a `"writing-tools"` counterpart to the mid-session
 * capability-disappears redirect that `"discover"` already gets
 * (`get().view === "discover" && !modelBrowserAvailable(...)`), redirecting
 * to `"chat"` instead of `"library"`. */
export function writingToolsAvailable(plugins: PluginInfo[]): boolean {
  return plugins.some((p) => p.capabilities.includes("writing-tools") && p.enabled);
}
```

No `plugins/PLUGINS.md` edit needed — explicit conclusion, not a silent
default. §8 already generically covers everything writing-tools needs
(manifest shape, TS gate expression, the setup/service non-check, and the
criterion for when a Rust-side `capability_enabled` check is warranted).
Adding "writing-tools" to the illustrative `(e.g. model-browser,
model-router)` example list in §8's header is optional cosmetic churn, not
required.

**No `PluginManager::capability_enabled("writing-tools")` check anywhere in
Rust** — explicit recommendation, cross-checked against both existing
precedents. §8's own criterion is: add the Rust-side check only when a
capability also has to gate a pre-existing, always-reachable code path with
no UI surface of its own (that's `model-router`'s case, gating the ambient
`classify_message`). `translate_stream`/`correct_stream` are net-new
commands reachable only from the plugin-gated screen — structurally
identical to `hf-browser`'s deliberately-ungated `list_hf_models`/
`add_hf_model` (docs/design/HF_BROWSER.md's decision 2: "the plugin gates the
*surface*, not the capability"). **Correction applied**: the original
analogy overstated why HF Browser's commands are ungated (they're not
net-new — they predate and also serve `SettingsSheet`) — the conclusion
("no capability_enabled check needed") still holds for writing-tools on its
own, correct grounds (net-new, single call site), just not via that
specific analogy.

`crates/core/src/plugin.rs` gets **two** test changes, both required (both
independently caught by two different areas):

```rust
/// The shipped writing-tools manifest is also python-less (see PLUGINS.md
/// §8): a pure UI-capability flag with no setup/service/chat of its own.
/// Unlike model-router, its capability gates a UI surface only; per
/// docs/design/HF_BROWSER.md's decision 2, translate_stream/correct_stream are
/// deliberately NOT gated by PluginManager::capability_enabled.
#[test]
fn parses_the_writing_tools_manifest() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/writing-tools/plugin.json");
    let raw = std::fs::read_to_string(path).expect("writing-tools plugin.json present");
    let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
    assert_eq!(m.id, "writing-tools");
    assert!(m.capabilities.iter().any(|c| c == "writing-tools"));
    assert!(m.python.is_none(), "writing-tools is python-less — pure Rust + UI");

    let p = DiscoveredPlugin { manifest: m, dir: PathBuf::from("/tmp/x") };
    assert!(!has_chat(&p));
    assert!(!has_service(&p));
    assert!(!provides(&p, "writing-tools"));
}
```

**Required, easy to miss**: `discovers_the_real_repo_plugins_including_hf_browser`
does an exact-match assertion — `assert_eq!(ids, ["hf-browser",
"model-router", "speech"]);` — against every non-core manifest found by
scanning the real `plugins/` directory. The instant
`plugins/writing-tools/plugin.json` exists, this test fails unless updated
to `["hf-browser", "model-router", "speech", "writing-tools"]` (alphabetical,
matching the existing sort), with a matching per-plugin assertion block
added for writing-tools (name, capabilities include "writing-tools",
`!enabled`, `SetupState::Ready`, `ServiceState::NotApplicable`, `!has_chat`)
mirroring the existing hf-browser/model-router blocks.

### D. Frontend UI: nav wiring, the screen, and the API wrappers

**Note the two live blockers that touch this area**: the i18n key-name
mismatch (#3) and the language wire-format mismatch (#4), both above. The
code below is given as researched — fix both before shipping it as-is.

`ui/src/state/store.ts`: `View` grows a 6th member —
`"chat" | "library" | "plugins" | "discover" | "skills" | "writing-tools"`
(no clash with any existing View literal or plugin capability string,
confirmed). Two mid-session capability-disappears redirects are needed —
**correction applied**: this document's earlier citation of a single
"`store.ts:895-922`" span for the discover→library fallback was imprecise;
it's actually two separate call sites (inside `loadPlugins`, and inside
`setPluginEnabled`'s optimistic-disable branch) — both need the analogous
addition:

```ts
// inside loadPlugins, right after the existing discover→library check:
if (get().view === "writing-tools" && !writingToolsAvailable(plugins)) {
  set({ view: "chat" });
}

// inside setPluginEnabled's optimistic-disable branch, right after the
// existing discover→library check:
if (get().view === "writing-tools" && !writingToolsAvailable(get().plugins)) {
  set({ view: "chat" });
}
```

`ui/src/views/Sidebar.tsx`: a new placeholder icon (flagged explicitly as
TBD, not a design decision — this document's Design section already said
"icon TBD"):

```tsx
const LanguagesIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M5 7h8M9 4.5v2.5M7 7c0 3 1.8 5.3 4.5 6.5M11.5 7c-.6 2-1.8 3.8-3.5 5" />
    <path d="M13 19l3.2-8 3.2 8M14.4 16.4h3.6" />
  </svg>
);
```

...and a new menu-item block cloned from the `browseAvailable` block,
placed immediately after it (grouping the two opt-in capability-gated nav
entries — Browse, Writing Tools — adjacently, ahead of the Plugins
management screen):

```tsx
{writingToolsOn && (
  <button
    type="button"
    className={`menu-item${view === "writing-tools" ? " sidebar-action-active" : ""}`}
    aria-current={view === "writing-tools" ? "page" : undefined}
    onClick={() => {
      setView(view === "writing-tools" ? "chat" : "writing-tools");
      closeMenu();
    }}
  >
    <LanguagesIcon /> {t("sidebar.writingTools")}
  </button>
)}
```

`ui/src/App.tsx`: the view-branching ternary needs an **explicit** new
branch (not a trailing `else`, since that `else` is only correct today by
elimination over a closed 5-member union):

```tsx
) : view === "skills" ? (
  <SkillsView />
) : view === "writing-tools" ? (
  <WritingToolsView />
) : (
  <Plugins />
)}
```

`ui/src/lib/api.ts`: two wrappers mirroring `chatStream`'s exact
`Channel<ChatEvent>` pattern (parameter names are a guess at the Rust
command's argument names — must match exactly whatever names the chosen
backend option above actually uses):

```ts
translateStream: (
  text: string,
  sourceLang: string | null,
  targetLang: string,
  onEvent: (e: ChatEvent) => void,
) => {
  const channel = new Channel<ChatEvent>();
  channel.onmessage = onEvent;
  return invoke<void>("translate_stream", { text, sourceLang, targetLang, onEvent: channel });
},
correctStream: (text: string, onEvent: (e: ChatEvent) => void) => {
  const channel = new Channel<ChatEvent>();
  channel.onmessage = onEvent;
  return invoke<void>("correct_stream", { text, onEvent: channel });
},
```

`ui/src/views/WritingToolsView.tsx` (new file) — key finding that shaped
this design: `chat_stream`'s `Channel<ChatEvent>` is **not** consumed inside
`ChatView.tsx` as this document's earlier "Verified technical foundation"
section implied — it's consumed entirely inside the `send` action in
`store.ts`. Since Writing Tools has no conversation/Library persistence
(unlike `send`), it consumes `api.translateStream`/`correctStream`'s
channel directly in this component's own local state instead of routing
through a new store action:

```tsx
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import type { ChatEvent } from "../lib/types";
import { useStore } from "../state/store";
import { WRITING_TOOL_LANGUAGES } from "../lib/writingToolsLanguages";

type Mode = "translate" | "correct";

export default function WritingToolsView() {
  const { t } = useTranslation();
  const engineStreaming = useStore((s) => s.streaming);

  const [mode, setMode] = useState<Mode>("translate");
  const [sourceText, setSourceText] = useState("");
  const [sourceLang, setSourceLang] = useState<string | null>(null); // null = Detect
  const [targetLang, setTargetLang] = useState<string | null>(null);
  const [resultText, setResultText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const requestSeq = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  const blockedByChat = engineStreaming && !busy;
  const trimmed = sourceText.trim();
  const canRun = trimmed.length > 0 && !busy && !blockedByChat && (mode === "correct" || !!targetLang);

  const switchMode = (next: Mode) => {
    if (busy) return;
    setMode(next);
    setResultText("");
    setError(null);
  };

  const run = async () => {
    if (!canRun) return;
    const seq = ++requestSeq.current;
    setBusy(true);
    setError(null);
    setResultText("");
    useStore.setState({ streaming: true }); // proposed extension, see note below

    const onEvent = (event: ChatEvent) => {
      if (!alive.current || requestSeq.current !== seq) return;
      switch (event.type) {
        case "token": setResultText((r) => r + event.data.text); break;
        case "error": setError(event.data.message); break;
        default: break; // token/done/error are the only variants these commands emit
      }
    };

    try {
      if (mode === "translate") {
        await api.translateStream(trimmed, sourceLang, targetLang as string, onEvent);
      } else {
        await api.correctStream(trimmed, onEvent);
      }
    } catch (err: unknown) {
      if (alive.current && requestSeq.current === seq) setError(String(err));
    } finally {
      if (alive.current && requestSeq.current === seq) setBusy(false);
      useStore.setState({ streaming: false });
    }
  };

  const stop = () => void api.stopGeneration(); // open question: does this generalize? see below

  const copy = async () => {
    if (await copyText(resultText)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    }
  };

  return (
    <div className="writing-tools-view">
      <div className="writing-tools-scroll">
        <div className="writing-tools-col">
          <div className="writing-tools-head">
            <h1>{t("writingTools.title")}</h1>
            <p>{t("writingTools.subtitle")}</p>
          </div>

          <div className="writing-tools-toggle" role="tablist" aria-label={t("writingTools.modeAriaLabel")}>
            <button type="button" role="tab" aria-selected={mode === "translate"}
              className={`writing-tools-toggle-btn${mode === "translate" ? " writing-tools-toggle-active" : ""}`}
              disabled={busy} onClick={() => switchMode("translate")}>
              {t("writingTools.modeTranslate")}
            </button>
            <button type="button" role="tab" aria-selected={mode === "correct"}
              className={`writing-tools-toggle-btn${mode === "correct" ? " writing-tools-toggle-active" : ""}`}
              disabled={busy} onClick={() => switchMode("correct")}>
              {t("writingTools.modeCorrect")}
            </button>
          </div>

          {mode === "translate" && (
            <div className="writing-tools-lang-row">
              <select value={sourceLang ?? ""} disabled={busy} aria-label={t("writingTools.sourceLangAriaLabel")}
                onChange={(e) => setSourceLang(e.target.value || null)}>
                <option value="">{t("writingTools.detectLanguage")}</option>
                {WRITING_TOOL_LANGUAGES.map((lang) => <option key={lang.id} value={lang.id}>{lang.name}</option>)}
              </select>
              <span aria-hidden="true">→</span>
              <select value={targetLang ?? ""} disabled={busy} aria-label={t("writingTools.targetLangAriaLabel")}
                onChange={(e) => setTargetLang(e.target.value || null)}>
                <option value="" disabled>{t("writingTools.chooseLanguage")}</option>
                {WRITING_TOOL_LANGUAGES.map((lang) => <option key={lang.id} value={lang.id}>{lang.name}</option>)}
              </select>
            </div>
          )}

          <div className="writing-tools-panes">
            <div className="writing-tools-pane">
              <label htmlFor="writing-tools-source">{t("writingTools.sourceLabel")}</label>
              <textarea id="writing-tools-source" value={sourceText} disabled={busy}
                placeholder={t("writingTools.sourcePlaceholder")}
                onChange={(e) => setSourceText(e.target.value)} />
            </div>
            <div className="writing-tools-pane">
              <div className="writing-tools-result-head">
                <label htmlFor="writing-tools-result">{t("writingTools.resultLabel")}</label>
                {resultText && (
                  <button type="button" className="btn-quiet" onClick={() => void copy()}>
                    {copied ? t("writingTools.copied") : t("writingTools.copy")}
                  </button>
                )}
              </div>
              <textarea id="writing-tools-result" value={resultText} readOnly placeholder={t("writingTools.resultPlaceholder")} />
            </div>
          </div>

          {blockedByChat && <p className="writing-tools-notice" role="status">{t("writingTools.engineBusy")}</p>}
          {error && <p className="writing-tools-error" role="alert">{error}</p>}

          <div className="writing-tools-actions">
            {busy ? (
              <button type="button" className="btn-ghost" onClick={stop}>{t("writingTools.stop")}</button>
            ) : (
              <button type="button" className="btn" disabled={!canRun} onClick={() => void run()}>
                {mode === "translate" ? t("writingTools.runTranslate") : t("writingTools.runCorrect")}
              </button>
            )}
          </div>

          <p className="writing-tools-local-note">{t("writingTools.localFirstNote")}</p>
        </div>
      </div>
    </div>
  );
}
```

`ui/src/lib/writingToolsLanguages.ts` (new file) — **resolved per blocker
#4**: ids are the wire value (matching Area A's `Language`/`LANGUAGES`
exactly — same ids, same names, same order), not the plain English display
strings this same frontend-ui area's own earlier draft used (below, for
comparison, is the corrected version — the earlier draft sent
`WRITING_TOOL_LANGUAGES.map((lang) => <option value={lang}>{lang}</option>)`
with `lang` itself a display string like `"French"`, which never matched
any `Language.id` the backend validates against). This is a hand-kept
duplicate of the Rust table (see "Reconciliation" above for why
that drift risk is an accepted v1 tradeoff, not solved here):

```ts
export interface WritingToolLanguage {
  id: string;
  name: string;
}

// Mirrors crates/core/src/writing_tools.rs's LANGUAGES exactly — same ids,
// same names, same order. Keep both lists in sync by hand; see
// docs/design/WRITING_TOOLS.md's "Open questions" for a possible future fix
// (serving this list from Rust instead of duplicating it).
export const WRITING_TOOL_LANGUAGES: readonly WritingToolLanguage[] = [
  { id: "en", name: "English" },
  { id: "es", name: "Spanish" },
  { id: "fr", name: "French" },
  { id: "de", name: "German" },
  { id: "it", name: "Italian" },
  { id: "pt", name: "Portuguese" },
  { id: "nl", name: "Dutch" },
  { id: "sv", name: "Swedish" },
  { id: "no", name: "Norwegian" },
  { id: "da", name: "Danish" },
  { id: "fi", name: "Finnish" },
  { id: "pl", name: "Polish" },
  { id: "cs", name: "Czech" },
  { id: "ro", name: "Romanian" },
  { id: "hu", name: "Hungarian" },
  { id: "el", name: "Greek" },
  { id: "ru", name: "Russian" },
  { id: "uk", name: "Ukrainian" },
  { id: "tr", name: "Turkish" },
  { id: "ar", name: "Arabic" },
  { id: "he", name: "Hebrew" },
  { id: "hi", name: "Hindi" },
  { id: "vi", name: "Vietnamese" },
  { id: "th", name: "Thai" },
  { id: "id", name: "Indonesian" },
  { id: "zh", name: "Chinese (Simplified)" },
  { id: "ja", name: "Japanese" },
  { id: "ko", name: "Korean" },
];
```

The two `<select>` option lists in `WritingToolsView.tsx` above already
reflect this (`<option key={lang.id} value={lang.id}>{lang.name}</option>`)
— `sourceLang`/`targetLang` state now hold an id (e.g. `"fr"`) rather than a
display string, which is exactly what `translateStream`'s `sourceLang`/
`targetLang` parameters and the backend's `language_name` validation above
already expect.

`ui/src/styles/writing-tools.css` (new file) — deliberately **not** reusing
`.chat-layout`: verified it's only 3 trivial CSS declarations with no
meaningful shared styling beyond that, so per the repo's real one-
stylesheet-per-view convention this gets its own file, reproducing the same
flex-row shape under its own class name and reusing the real design tokens
(`--paper`, `--ink`, `--line`, `--radius-*`, etc.) and existing button
classes (`.btn`/`.btn-ghost`/`.btn-quiet`) rather than inventing new ones.
Full proposed CSS is in the workflow's research output; omitted here for
length — it follows `skills.css`'s page-shell pattern (`.skills-view`/
`.skills-scroll`/`.skills-col`/`.skills-head`) plus a two-pane flex layout
for the source/result textareas.

`ui/src/main.tsx`: one import line, `import "./styles/writing-tools.css";`,
next to the other per-view stylesheet imports.

**Unlocked proposals, flagged as such, not settled**: (1) the bidirectional
`streaming` mutex (`useStore.setState({ streaming: true/false })` around
this view's own request, so the *existing* chat-side guards transparently
also block chat while Writing Tools is generating) broadens what
`streaming` means app-wide — needs explicit sign-off, and if adopted,
`streaming`'s doc comment in `store.ts` should note the broadened meaning;
now that the backend's busy-check is real (adopted design above, not the
rejected no-busy-check alternative), this frontend flag is a UX nicety
(disabling chat's own send button proactively) layered on top of a real
backend guarantee, not a substitute for one. (2) Whether
`api.stopGeneration()` actually generalizes to a non-conversation stream, or
silently no-ops, is unverified — blocks whether the Stop button above
actually works.

### E. Localization strings, tests, and CI status

**Resolved per blocker #3**: flat keys, matching every `t("writingTools.*")`
call `WritingToolsView.tsx` above actually makes — the nested shape
(`mode.translate`, `language.detect`, …) from the original draft is dropped
entirely, and three keys the draft defined but the component never calls
(`working.*`, `genericError`, `smallModelHint`) are dropped too, for the same
reason `emptyTextError`/`missingTargetLanguageError` were already dropped
below: this app's command-layer already owns friendly-error copy (English,
unlocalized, same as every other command's `Result<_, String>` errors — see
`translate_stream`/`correct_stream` above), so a duplicate, unused, or
would-be-inconsistent i18n copy is dead weight, not a hedge.

New `"writingTools"` block for `en.json` (inserted after `skillsView`
closes, before `markdown` begins), plus one `sidebar.writingTools` key —
exactly the 20 keys the component calls, nothing more:

```json
"writingTools": {
  "title": "Writing Tools",
  "subtitle": "Translate text or fix grammar and style, right here — using the AI model that's already running on your Mac.",
  "modeAriaLabel": "Choose Translate or Correct",
  "modeTranslate": "Translate",
  "modeCorrect": "Correct",
  "detectLanguage": "Detect language",
  "sourceLangAriaLabel": "Translate from",
  "targetLangAriaLabel": "Translate into",
  "chooseLanguage": "Choose a language",
  "sourceLabel": "Your text",
  "sourcePlaceholder": "Paste or type your text here…",
  "resultLabel": "Result",
  "resultPlaceholder": "Your result will appear here.",
  "copy": "Copy",
  "copied": "Copied",
  "engineBusy": "Your AI is busy with another reply right now — this will run as soon as it's free.",
  "stop": "Stop",
  "runTranslate": "Translate",
  "runCorrect": "Correct",
  "localFirstNote": "Nothing you type here leaves your Mac."
}
```

`fr.json` needs the exact same key *shape* (`ui/src/i18n/keys.test.ts`
asserts en/fr declare identical flattened key sets and fails the build
otherwise) with real French copy, e.g. `"title": "Outils de rédaction"`,
`"subtitle": "Traduisez du texte ou corrigez la grammaire et le style, ici
même — avec le modèle d'IA déjà lancé sur votre Mac."`,
`"modeTranslate": "Traduire"`, `"modeCorrect": "Corriger"`,
`"localFirstNote": "Rien de ce que vous tapez ici ne quitte votre Mac."` —
full key-for-key French copy for the remaining keys was proposed in the
workflow's research output and should be carried over verbatim (translation
quality, not structure, is what's left to verify there).

Test enumeration for the whole feature: a `writing_tools.rs` unit-test
module (Area A, already shown in full above); `parses_the_writing_tools_manifest`
plus the `discovers_the_real_repo_plugins_including_hf_browser` fix (Area
C, already shown in full above); a new `describe("writingToolsAvailable",
...)` block in `ui/src/state/store.test.ts`, mirroring the existing
`modelBrowserAvailable` block byte-for-byte (five cases: empty, enabled,
disabled, other-plugins-only, mixed); **no** `WritingToolsView.test.tsx` —
confirmed no view-level `.test.tsx` convention exists anywhere in this repo
(`ChatView.tsx`/`SettingsSheet.tsx` have none either), so this feature
shouldn't introduce one; and one new Playwright mocked-e2e scenario,
`ui/tests/e2e/writing-tools.spec.ts`, modeled on the repo's only existing
spec (`onboarding.spec.ts`) — **with an explicit, verified limitation**:
`tauriMock.ts`'s own docstring says the mock does not implement the
`Channel`/`transformCallback` protocol `chat_stream` (and therefore
`translate_stream`/`correct_stream`) use for streaming, so this scenario can
only cover plugin-gating, navigation, and the static UI shell — not an
actual streamed translate/correct round trip. That gap is closed instead by
the `#[ignore]`d live-engine Rust test called for in Phase 2 below.

**CI status — re-confirmed, still broken, independent of this feature**:
`main`'s latest push-triggered run (`33061223665`, commit `3257d46`,
2026-08-27) still fails both `frontend` (`npm ci` EUSAGE — `ui/package-lock.json`
out of sync) and `rust` (`cargo fmt --check` on `crates/app/src/commands.rs`).
This is a real prerequisite for this feature's own CI to ever go green, not
something this plan can route around — flagging again per this document's
"CI status" line above.

### Closing note from the cross-area critique (as raised — see Reconciliation for how each was closed)

*"Do not proceed straight to implementation from this bundle — fold it into
one reconciled plan first. The single most load-bearing item is the
`writing_tools.rs` public interface: pick ONE contract and rewrite the
Tauri-command-layer code against it. Equally load-bearing: pick one of the
two incompatible `translate_stream`/`correct_stream` implementations and, per
Option 1's own verify finding, extend whichever is chosen so the existing
`plain_chat`/`cloud_chat_turn`/`agent_chat_turn`/`web_search_chat_turn` turn-
runners also honor the same claim — otherwise the busy/cancel race this whole
exercise was meant to close remains open in one direction. Before any
frontend work lands, reconcile the `WritingToolsView.tsx` i18n key names
against whichever locale-file shape ships, and reconcile the language-list
value format end-to-end. Deduplicate the several file edits two areas each
independently authored. Finally, add the two doc-mandated items no area
produced: an actual Phase-0 rebase task (plus a fix for main's already-red
CI), and Phase 2's `#[ignore]`d live-engine streaming test."*

Every item in that critique is now addressed above (see "Reconciliation")
except the two "doc-mandated items no area produced" — those become actual
Phase 0/Phase 2 tasks below, since scheduling work is what the Phases section
is for, not something to resolve inline in a code-reconciliation pass.

## Phases

Every phase closes out with `cargo test --workspace` and `graphify update .`
(repo convention).

Each phase below is a summary; the "Verified implementation detail" section
above has the actual proposed code, already reconciled against the four
blockers the six-area research pass surfaced (see "Reconciliation" there for
what was decided and why). Phase 0 and part of Phase 2 were gaps in the
original version of this plan (no research area had actually scheduled them
as work) — added below now that they've been identified.

### Phase 0 — ✅ Done (2026-08-29) — Fix `main`'s CI, then rebase

- `main`'s CI (lockfile + fmt) turned out to already be fixed upstream by the
  time this phase started — confirmed via the latest push-triggered run on
  `origin/main` (`33209292049`, commit `6053160`, "Remote Access" #67): both
  `frontend` and `rust` jobs green. No fix needed from this feature.
- Rebased this branch onto `origin/main` (`6053160`), clean, no conflicts.
  **Caveat**: the `EngineBackend`/`SubprocessBackend`/`EngineManager` split
  (`b4ab5cc`/`02fde75`/`0aa2b83`) is still *not* on `origin/main` as of this
  writing — it exists only as an unpushed local merge on this machine, owned
  by a concurrent session actively working in that area. Rebasing onto
  someone else's unpushed local branch state would be unsafe (it can still
  be rewritten out from under this branch), so this phase rebased onto
  `origin/main` only. Phase 1 doesn't touch engine internals at all, so this
  doesn't block it; **Phase 2 will need to check again** whether that refactor
  has landed on `origin/main` before it starts (see that phase's own note).

### Phase 1 — ✅ Done (2026-08-29) — Core request builders (pure Rust, no UI, no streaming yet)

- `crates/core/src/writing_tools.rs` landed exactly as reconciled above:
  `translate_system_prompt`, `CORRECT_SYSTEM_PROMPT`, `fixed_sampling`,
  `language_name`/`Language`/`LANGUAGES`, plus the `pub mod writing_tools;`
  insertion (alphabetically last) and module-doc bullet in
  `crates/core/src/lib.rs`.
- **One real bug found during implementation, not caught by planning**: the
  self-referential leak-detection test's needle (a bare two-word snake_case
  string) matched this file's own `translate_system_prompt` function name —
  exactly the fragility this document's own "Risks specific to this area"
  note above predicted, just realized in practice the moment the function
  got that name. Fixed by narrowing the needle to a dot-prefixed field-access
  pattern (`.field_name`, not the bare name), which the real leak shape
  (`settings.<field>`) has and a function name never does; then hit the same
  trap a second time writing the doc comment *explaining* the fix (which
  spelled the needle out in prose) — fixed by describing the check
  abstractly instead of quoting it. Both fixes are in the file's own doc
  comments now, as a warning to whoever touches this test next.
- Verified: 8 unit tests passing, `cargo fmt --check` clean, `cargo clippy
  -p openpal-core --lib -- -D warnings` clean, `cargo test --workspace`:
  482 passed / 0 failed / 3 ignored (pre-existing ignores, unrelated).

### Phase 2 — ✅ Done (2026-08-29) — Streaming plumbing + commands

- **Checked before starting, as flagged**: `origin/main` still hadn't picked
  up the `EngineBackend`/`SubprocessBackend`/`EngineManager` split as a
  separate landing — but it turned out to already be *on* `origin/main`
  anyway, folded into the "Remote Access" (#67) merge Phase 0 already
  rebased onto (`crates/core/src/engine/` is a directory now: `mod.rs` +
  `subprocess.rs`, an `EngineBackend` trait + `SubprocessBackend` impl —
  confirmed by reading the actual files, not assumed). `EngineManager::
  chat_stream`'s signature is exactly what this document's Backend section
  assumed, so nothing about the design needed to change; no further rebase
  was needed.
- `try_claim_generation_slot`/`claim_generation_slot` + `run_writing_tool_stream`
  landed in `crates/app/src/commands.rs`, and — the one piece of this phase
  that touches pre-existing code, not just new code — all **four** existing
  turn-runners (`plain_chat`, `cloud_chat_turn`, `agent_chat_turn`,
  `web_search_chat_turn`) now claim `state.chat_cancel` through the same
  helper instead of assigning it directly, closing the busy race in both
  directions as planned. `cloud_chat_turn`'s entitlement/token logic was
  also extracted into `resolve_cloud_access` so `run_writing_tool_stream`
  reuses it verbatim, with no behavior change to `cloud_chat_turn` itself.
- `translate_stream`/`correct_stream` landed exactly as designed: input
  validation (extracted into `validate_writing_tool_text`/
  `validate_translate_languages` so it's actually unit-testable) runs before
  `parental::enforce_send`, backend selection (local/cloud) via
  `run_writing_tool_stream`, both registered in `generate_handler!` next to
  `chat_stream`/`stop_generation`.
- Tests, all genuinely run (not just compiled): 8 new unit tests
  (`validate_writing_tool_text`/`validate_translate_languages`'s cases, and
  `claim_generation_slot`'s rejection-then-recovery path against a bare
  `Mutex` — `AppState` itself isn't constructible in this crate's tests, so
  the claim logic was factored out specifically to make this possible); two
  new `#[ignore]`d live-engine tests in a new `crates/core/tests/
  writing_tools_e2e.rs` (translate and correct, each asserting real streamed
  `Token`s and a terminating `Done` against the actual llama-server binary +
  the tiny SmolLM2 test model — genuinely run, not just written, after
  staging the engine with `cargo run -p openpal-bundler -- fetch-engine`);
  the shared engine-startup boilerplate was factored out of `engine_e2e.rs`
  into `crates/core/tests/support/mod.rs` so the new file didn't have to
  duplicate it (`engine_e2e.rs` itself re-verified passing after that
  refactor).
- Verified: `cargo fmt --check` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test --workspace`: 482 + 40 +
  46 passed / 0 failed / 3 ignored (unrelated) + 3 ignored (the new
  live-engine tests, all separately run for real and passing).

### Phase 3 — ✅ Done (2026-08-29) — Plugin + UI skeleton

- `plugins/writing-tools/plugin.json` + README, `parses_the_writing_tools_manifest`,
  and the fix to `discovers_the_real_repo_plugins_including_hf_browser`'s
  exact-match assertion all landed exactly as designed. One real polish fix
  made during implementation: the manifest's description used literal `--`
  where every sibling plugin (`hf-browser`, `model-router`) uses a real em
  dash "—" — corrected for consistency.
- `writingToolsAvailable`, the `View` union member, the `App.tsx` branch,
  the Sidebar "more" menu entry, and both mid-session
  capability-disappears redirects (confirmed as two separate call sites,
  exactly as flagged above) all landed as designed.
- `ui/src/views/WritingToolsView.tsx` + `ui/src/styles/writing-tools.css` +
  `ui/src/lib/writingToolsLanguages.ts` + the `api.ts` wrappers all landed.
  One inaccuracy in this document's own citation was caught and corrected
  during implementation: the StrictMode-safe alive-ref/sequence-ref pattern
  actually lives in `Discover.tsx`, not `Library.tsx` (`Library.tsx` has no
  such pattern at all) — the component was built against the real pattern.
- `en.json`/`fr.json` got exactly the 20-key flat `writingTools` block plus
  `sidebar.writingTools`, in both locales; `ui/src/i18n/keys.test.ts`
  (en/fr key-set parity) passes.
- `store.test.ts` got the five-case `writingToolsAvailable` describe block,
  mirroring `modelBrowserAvailable`'s exactly.
- **Descoped, deliberately, not silently skipped**: no Playwright mocked-e2e
  scenario. This repo's entire e2e harness (`ui/tests/e2e/`) currently
  supports exactly one scenario — fresh-install onboarding
  (`onboarding.spec.ts` + `fixtures.ts`'s only export,
  `freshInstallResponses`) — with no fixture at all for "a returning user
  past onboarding, in Chat, with plugins enabled." Building that fixture
  (mocking every command the post-onboarding boot sequence calls) is real,
  separate test-infrastructure work, disproportionate to this feature.
  Coverage instead comes from the Rust plugin tests, the `store.test.ts`
  unit tests, and manual verification (below) — a real gap, not a hidden one.
- **Implementation approach**: four parallel agents (Rust plugin manifest;
  i18n; nav wiring — `store.ts`/`Sidebar.tsx`/`App.tsx`/`store.test.ts`; the
  UI component surface — `api.ts`/`WritingToolsView.tsx`/
  `writingToolsLanguages.ts`/CSS/`main.tsx`), each owning a disjoint file
  set and working from the already-fully-reconciled spec above (unlike the
  original six-area planning pass, there were no open design decisions left
  to make incompatibly — this was transcription, not design). All four
  integrated cleanly on the first pass: zero cross-agent contradictions,
  confirmed independently afterward (below), not just by the agents' own
  reports.
- **Verified, independently, after the fact** (re-run fresh, not trusted
  from agent reports): `cargo fmt --check` clean; `cargo clippy --workspace
  --all-targets -- -D warnings` clean; `cargo test --workspace` —
  483 + 40 + 46 passed / 0 failed / 3 ignored (unrelated); `npx tsc --noEmit`
  from `ui/` — 0 errors; `npm run lint` — 0 errors, 73 warnings (2 new, in
  `WritingToolsView.tsx`, both `jsx-a11y/label-has-for` — the identical
  warning already exists in `SkillsView.tsx`/`SettingsSheet.tsx`, an
  accepted pre-existing pattern, not a new regression in rigor); `npx vitest
  run` — 154/154 passed. Also reverted one incidental, unintended
  `ui/package-lock.json` diff (pure `"peer": true` metadata churn from a
  local `npx` invocation, no real dependency change) that wasn't part of
  any agent's assignment, and re-confirmed `npm ci` still installs cleanly
  from the untouched lockfile.
- **Manual app verification — honest about its real limits**: rebuilt and
  relaunched the actual app (`npm run dev`) with every Phase 1-3 change
  compiled in; it launched and ran without a crash or panic. A full visual
  click-through (enable the plugin from Add-ons, confirm the sidebar entry
  appears, navigate to the screen, run a real translation/correction) was
  **not possible in this environment**: both a plain shell's
  `screencapture`/`osascript` and Orca's own computer-use surface report
  Accessibility/Screen Recording permission as not granted here — a
  one-time grant only a human at the machine can make. This is a real,
  stated gap, not a claimed screenshot that doesn't exist. The dev server
  was left running afterward in case a human wants to pick up the
  interactive check.

### Phase 4 — ✅ Done (2026-08-29) — Document translation

Confirmed scope up front (asked, not assumed, since the two realistic
options — plain-text-out vs. original-formatting-preserving — are wildly
different engineering efforts): **upload + download, plain text out**. A
translated PDF does not come back as a PDF; this app's attachment pipeline
only ever extracts text, and reformatting a `.docx`/`.pdf` back into its
original layout is real structural document editing this app has no
infrastructure for at all.

Turned out to need almost no new backend surface, because the existing
attachment pipeline already does the hard part:

- **Picking + extracting text**: reuses the existing `pick_attachments`
  command (`crates/app/src/commands.rs`) completely unchanged — it already
  extracts text from PDFs and plain-text files for chat attachments (up to
  `MAX_DOC_CHARS = 100_000`, `crates/core/src/attachments.rs`). The frontend
  calls `api.pickAttachments(false)` (no images — translation is text-only)
  and takes the first `kind: "document"` result's `.text`.
- **Chunking**: `ui/src/lib/writingToolsChunking.ts`'s `chunkText(text,
  maxChunkChars = 4000)` — paragraph boundaries first, sentence boundaries
  within an oversized paragraph, a hard character slice as the last resort
  for a single punctuation-free run that still doesn't fit. A fixed,
  conservative constant rather than querying the running model's actual
  context length — computing "how much of the context window is actually
  left after the system prompt and sampling overhead" precisely is real
  complexity for marginal benefit; a beginner never sees or tunes this
  number. `WritingToolsView.tsx`'s `run()` loops chunks sequentially,
  awaiting each one before starting the next and appending its streamed
  tokens (joined with `\n\n` between chunks) into the same growing result —
  **no new Rust streaming/orchestration code at all**: each chunk is just
  another ordinary call to the exact same `translateStream`/`correctStream`
  Phase 2 already shipped. A `{current, total}` chunk-progress notice shows
  above the action row whenever a document actually splits into more than
  one chunk, honestly noting that quality may vary slightly at section
  breaks (independent chunks have no cross-chunk context) — this app's
  established hedged-estimate voice (fit badges, disk-offload copy).
- **Saving the result**: one new Rust command,
  `save_writing_tool_result(app, text, suggested_file_name) -> Result<bool,
  String>`, mirroring the existing `download_artifact` command's native
  save-dialog pattern exactly (`Ok(false)` if the dialog is closed without a
  choice, not an error) with `export_logs`'s `spawn_blocking`-wrapped write
  for a possibly-large document. The frontend only offers "Download" when
  the source text came from an upload (`uploadedFileName` is set) — for
  pasted text, Copy is the natural output; suggested filenames are the
  original name (extension stripped) plus `" (<target language>)"` for
  Translate or `" (corrected)"` for Correct, as `.txt`.
- Uploading clears any previous result/diff/error, matches the existing
  `switchMode` reset pattern; manually clearing the source box back to empty
  also clears the remembered upload (an ordinary edit does not — only an
  empty box means "this isn't the uploaded document's text anymore").

### Phase 5 — ✅ Done (2026-08-29) — Tracked-changes diff view for Correct

Confirmed scope up front: **full per-change accept/reject** (the more
thorough of two options, deliberately chosen over a simpler read-only
diff). Correct-mode only — Translate's output is a different language
entirely, so a word-level diff against the source is meaningless there; the
plain result textarea is unconditional for Translate.

- **Diff engine**: added the `diff` npm package (`diffWordsWithSpace`) — no
  equivalent existed in this codebase, and hand-rolling a word-level diff
  algorithm for this is exactly the kind of well-solved problem not worth
  reinventing. `ui/src/lib/writingToolsDiff.ts` wraps it: `computeWordDiff`
  groups `diff`'s raw removed/added/equal parts into `DiffPart[]` — a
  `removed` part immediately adjacent to an `added` part (either order)
  becomes one replacement `DiffChange` with a stable id; a lone `removed` is
  a pure deletion, a lone `added` a pure insertion. `reconcileDiffText(parts,
  accepted)` walks the parts and picks `added` or `removed` per change
  according to the caller's current accept state (missing from the map =
  accepted, since corrections are accepted by default — that is the whole
  point of Correct mode). `defaultAcceptedMap` seeds "everything accepted."
- **When it runs**: the diff is computed once, on the *finished* result
  (`ChatEvent`'s `done`, effectively — after the chunk loop completes, not
  incrementally per token). Diffing a partial stream would visibly thrash as
  more tokens arrive; the plain textarea keeps showing the live stream while
  busy, and only Correct mode's *finished* state switches to the diff view.
  Works identically whether the correction came from pasted text or a
  chunked multi-part document — the diff is computed over the whole
  reassembled result either way, no special-casing.
- **UI**: each change renders inline as a `<del>` (struck through) next to
  an `<ins>` (underlined) — whichever side is *not* currently in effect is
  dimmed to `--ink-faint` so the accepted/live side reads clearly at a
  glance — plus one small icon button per change that flips its accept
  state (↺ to revert an accepted correction back to the original, ✓ to
  re-apply a rejected one), a real `<button>` so it's natively keyboard-
  operable. "Accept all"/"Reject all" bulk actions sit above the diff for
  convenience, alongside Copy/Download — both of those now act on
  `reconcileDiffText(diffParts, accepted)` once a diff exists, not the raw
  model output, so the accept/reject state is exactly what gets copied or
  saved.
- **Implementation approach**: `writingToolsDiff.ts` (this phase) and
  `writingToolsChunking.ts` (Phase 4) were built by two agents in parallel,
  each owning disjoint new files with zero design ambiguity left to resolve
  (both were fully specified pure-function contracts up front); the actual
  `WritingToolsView.tsx` integration — where both phases' state and
  rendering logic interleave in one file — was done by hand afterward
  rather than parallelized, specifically to avoid two agents editing the
  same file's interdependent regions concurrently.
- Verified, independently, after the fact: `cargo fmt --check`, `cargo
  clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
  (483+40+46 passed, 3 ignored unrelated) all clean; `npx tsc --noEmit` (0
  errors), `npm run build` (succeeds — the pre-existing chunk-size-warning
  is unrelated to this change), `npx eslint` (0 errors, 2 new warnings —
  the same pre-existing `jsx-a11y/label-has-for` pattern already accepted
  elsewhere in this codebase), `npx vitest run` (180/180 passed, including
  26 new tests across `writingToolsChunking.test.ts` and
  `writingToolsDiff.test.ts`). Rebuilt and relaunched the real app — `tauri
  dev`'s file watcher auto-rebuilt on every Rust and frontend change with
  zero panics/errors across the whole session, and this run actually
  reached a fully booted, hardware-detected state (unlike the Phase 3
  verification run, which hit a keychain hiccup) — still short of an actual
  visual click-through, which remains blocked on the same
  Accessibility/Screen Recording permission gap noted in Phase 3.

## Risk register

| Risk | Severity | Mitigation |
| --- | --- | --- |
| `main`'s in-flight `EngineBackend` refactor landing after this feature starts, forcing rework | Low (was Medium) | Verified moot: `EngineManager::chat_stream`'s signature is byte-for-byte unchanged pre/post-refactor and the adopted design lives entirely in `crates/app/src/commands.rs`, which the refactor doesn't touch (`git diff HEAD main -- crates/app/` is empty) — Phase 0 still rebases first, but there's nothing left to redo |
| Single inference slot: a Translate/Correct request contends with an in-progress chat reply, or vice versa | Low (was Medium) | Resolved by design: `try_claim_generation_slot`'s atomic check-and-claim, adopted for both the new commands and the four existing turn-runners (see "Reconciliation" → blocker #2) — reject-up-front with a friendly error, not a silent no-op or an unverified assumption about queuing |
| Small local models (4-8B) producing mediocre translations, especially for lower-resource languages | Medium | Curated, honest language list; no overpromising copy; a "results may vary with smaller models" note, consistent with this app's existing hedged-estimate voice (fit badges, disk-offload copy) |
| `settings.system_prompt` accidentally leaking into a translate/correct request via a shared helper | Medium | New commands build their own fixed system prompt from scratch (finding 5); unit test asserts `settings.system_prompt` is never referenced in `writing_tools.rs` |
| Parental controls bypass: a translate/correct tool could be used to produce content a chat-side filter would have blocked | Low-Medium | `parental::enforce_send` called from both new commands, same as `chat_stream` |
| Cloud-backend inconsistency: tool silently uses local engine while chat is configured for cloud, or vice versa | Low | New commands read `settings.chat_backend` and branch the same way `chat_stream` does (finding 4b) |
| Independent per-chunk translation loses cross-paragraph context at chunk boundaries | Low | Honest chunk-progress copy ("quality may vary slightly at section breaks"); paragraph/sentence-aware splitting minimizes mid-thought cuts; only triggers for documents actually long enough to need more than one chunk |

## Open questions

- Exact plugin/sidebar display name: "Writing Tools" (recommended, covers
  both modes under one label) vs. separate mentions of "Translate" and
  "Correct" in the description copy only?
- Curated language list membership — start from the catalog's most-tested
  languages, or a fixed DeepL-like list regardless of per-model track record?
- Should "last used target language" persist at all (small local setting), or
  should the tool always reset to a neutral default on open, favoring
  simplicity over convenience?
