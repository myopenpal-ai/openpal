# Accessibility remediation plan — OpenPal UI

Companion to [ACCESSIBILITY_AUDIT.md](ACCESSIBILITY_AUDIT.md), which this plan's file/line
references and WCAG/RGAA citations are drawn from.

OpenPal exists to get non-technical users onto local-first LLMs — that audience overlaps heavily with people who rely on a keyboard, a screen reader, or reduced-motion settings rather than a mouse and default OS animation. An invisible focus trap in the "More" menu or an unlabeled composer textarea isn't power-user polish here; it's the same class of bug as a broken install button, and it hits the exact beginner audience the product is built for. Seven of the findings below are blocker-severity — an unannounced streaming reply, an unreachable tool-consent gate for dangerous actions, three modal dialogs (Settings, Sign In, Update) a keyboard/AT user can get lost behind, an unannounced onboarding download, and contrast failures on the "will this model run on my machine" badge — meaning core first-run and daily-use flows are currently broken, not just under-polished, for keyboard and assistive-tech users. None of the fixes below need gating behind Settings → Advanced (accessibility is baseline usability, not an advanced capability) and none require a second model — every finding is a markup/CSS/focus-management fix within the single loaded model's existing UI.

## Phase 0 — Quick wins

Low-effort, low-risk fixes that land as labelling, token, and small-attribute changes — no new interaction logic, no dialog/focus-trap work. Sized for one or two PRs.

**Secondary text/icon contrast (`--ink-faint` token)**
- `ui/src/styles/global.css:13,73` — darken `--ink-faint` in both themes until it clears 4.5:1 for text / 3:1 for icons against `--paper`/`--paper-raised`/`--paper-sunken`, or restrict it to decorative/large-text use and move real secondary text to `--ink-soft` (already passes, ~5.5–6.9:1). One token change cascades to every use-site below — do it as a single PR:
  - `ui/src/styles/chat.css:114` (`.conv-when`), `:318` (`.app-version`), `:1187` (`.icon-btn` idle), `:1472-1476` (`.settings-value`)
  - `ui/src/styles/onboarding.css:90-93,95-104,131-137` (`.ob-reassure`, `.ob-update-choice`, `.ob-stat-label`)
  - `ui/src/styles/library.css:125-128,142-146` (`.model-meta`, `.model-why`), `:263-266` (`.settings-hint`)
  - `ui/src/styles/discover.css:129-132,142-146,200-203` (`.discover-card-owner`, `.discover-card-fit-hint`, `.model-page-note`)
  - `ui/src/styles/plugins.css:43-50,82-85` (`.plugins-empty`, `.plugin-meta`); `ui/src/styles/skills.css:45-52` (`.skills-empty`)
  - WCAG 1.4.3, RGAA 3
- `ui/src/styles/global.css:309-314` — fit-status badge text (`--warn`, `--ok`, `--ink-faint` on their `-soft`/sunken backgrounds) fails 4.5:1 at 12px/600-weight (2.44–4.18:1 measured); this is the single most important status on every model card (`Library.tsx:386`, `Discover.tsx:228-230,435-437`) — **blocker**. WCAG 1.4.3, RGAA 3.

**Reduced motion**
- No stylesheet in the project has a `prefers-reduced-motion` query. Add one `@media (prefers-reduced-motion: reduce)` block (in `global.css`, next to the existing keyframes at `:370-393`) that disables/shortens:
  - `chat.css` — `.caret` (579-587), `.thinking-dot` (595-601), `.searching-spinner`/`@keyframes spin` (684-696), `.trace-live` (853-860), `.composer-tool.is-recording`/`pulse-rec` (1280-1290), `pulse-dot` (used on the engine status dot, 186)
  - `onboarding.css:273-277` (`.ob-waking`/`breathe`); consider also capping its infinite loop
  - `plugins.css:179-189` (`.plugin-progress.is-indeterminate` sweep)
  - `library.css:217-222` (`.model-progress-bar` width transition), `discover.css:97` (card border-color transition)
  - WCAG 2.2.2, RGAA 13 (topic 13, not 10 — this is "Consultation" not "Présentation") for the infinite-animation items; RGAA 10 for the low-amplitude transitions

**Sidebar keyboard focus**
- `ui/src/styles/chat.css:117-137` / `ui/src/views/Sidebar.tsx:131-138` — `.conv-delete` is `opacity:0` revealed only on `:hover`, with no focus rule, so a tabbing keyboard user lands on an invisible destructive button. Add `.conv-row:focus-within .conv-delete, .conv-delete:focus-visible { opacity: 1; }`, mirroring the existing `.msg-actions:focus-within` rule already at `chat.css:513`. WCAG 2.4.7, RGAA 12.

**Focus-indicator contrast (token-level)**
- `ui/src/styles/discover.css:100-108` — `.discover-card:focus-visible` ring is ~1.13:1 (computed), the *only* keyboard cue for the whole Discover grid (cards are `role="button"`/`tabIndex=0`). WCAG 2.4.7, RGAA 12.
- `ui/src/styles/plugins.css:152-154` — plugin toggle focus ring, same ~1.13:1 low-alpha `--accent-soft` problem. WCAG 2.4.13, RGAA 3.
- `ui/src/styles/global.css:267-283` — global `textarea, input[type="text"]:focus` ring (drives the Skills editor fields among others) is ~1.13–1.64:1. WCAG 2.4.13, RGAA 3.
- `ui/src/styles/chat.css:1340-1342,1231-1233` — composer textarea strips its own focus box-shadow, leaving only a border-color change as the sole indicator; verify contrast/thickness. WCAG 2.4.7, RGAA 10.
- Recommend one opaque `--focus-ring` token (≥3:1 against surrounding backgrounds in both themes) replacing `--accent-soft` in all four places above, rather than four separate color picks.

**Missing/unreliable form labels**
- `ui/src/views/ChatView.tsx:1580-1619` — composer `<textarea>` has no persistent `aria-label` (only a `placeholder` that changes with engine state and disappears once typed). WCAG 3.3.2, RGAA 11.
- `ui/src/views/ChatView.tsx:895-917` — inline "edit message" `<textarea>` has no accessible name at all. WCAG 3.3.2, RGAA 11.
- `ui/src/views/Discover.tsx:156-172` — search `<input>` has only a placeholder; sort `<select>` has no label at all. WCAG 3.3.2, RGAA 11.
- `ui/src/views/Plugins.tsx:77-90` — enable/disable switch's accessible name lives only in a `title` on the wrapping `<label>` (empty name-from-content); put `aria-label` directly on the `<input>`. WCAG 4.1.2, RGAA 11.
- `ui/src/views/Library.tsx:249-257` — model-picker `<input type="range">` has `aria-label` but no `aria-valuetext`; screen readers hear only the raw index, not the model name. WCAG 4.1.2, RGAA 11.

**Ambiguous repeated button names**
- `ui/src/views/Library.tsx:450-455,480-492,536-543` — "Cancel"/"Remove"/"Discard" repeat identically across every card in a grid with nothing tying the name to the model. Add per-row `aria-label` including the model name (Library.tsx currently has exactly one `aria-label` in the whole file). WCAG 2.4.6, RGAA 11.

**`aria-current` for active state**
- `ui/src/views/Sidebar.tsx:116-121,161-205` (backed by `chat.css:323-326`) — active conversation and active view menu item are CSS-only (`conv-row-active`, `sidebar-action-active`). Add `aria-current="true"`/`"page"` driven by the same boolean already used for the class. WCAG 4.1.2, RGAA 12.

**Disclosure widgets missing `aria-controls`/`aria-expanded`**
- `ui/src/views/ChatView.tsx:264-279,310-347,365-393` — ThinkingPanel/ToolActivity/AgentActivity toggles set `aria-expanded` but never `aria-controls`, and the disclosed regions have no `id`. WCAG 1.3.1, RGAA 7.
- `ui/src/views/Onboarding.tsx:165-172,176-206` — "Pick a different model" toggle has no `aria-expanded` at all, and `.ob-alts` has no `id`/`aria-controls` link. WCAG 4.1.2, RGAA 7.

**Mislabeled/mismatched roles (one-line swaps)**
- `ui/src/views/ChatView.tsx:1090` — EngineBanner's failed-engine banner uses `role="status"`; should be `role="alert"` (keep `status` for the genuinely non-urgent `AutoUnloadTooltip.tsx:102`). WCAG 4.1.3, RGAA 7.
- `ui/src/views/ChatView.tsx:1053` — turn-level `chat-error` div has no `role="alert"`. WCAG 4.1.3, RGAA 7.
- `ui/src/views/ChatToolbar.tsx:393-407` — trigger declares `aria-haspopup="dialog"` but the popup (`.toolbar-panel`, line 407) has no role at all; add `role="dialog"` (non-modal) or drop the `aria-haspopup` value. WCAG 4.1.2, RGAA 7.

**Keyboard access to scrollable content**
- `ui/src/components/Markdown.tsx:81-113` (CodeBlock `<pre>`), `:141-145` (TableWrap) — both scroll horizontally (`chat.css:1118-1123`, `:1046-1048`) but are not focusable. Add `tabIndex={0}` (+ `aria-label`, e.g. "Code, scrollable" / "Table, scrollable"). WCAG 2.1.1.

**Heading structure (simple level swaps)**
- `ui/src/views/Library.tsx:75,111,142,385` — per-card model-name `<h2>` sits as a sibling of the section `<h2>` ("Other models"/"Cloud models") instead of nested under it; bump to `<h3>`. WCAG 1.3.1, RGAA 9.
- `ui/src/views/Onboarding.tsx:139-151` — "recommend" scene has no `h1` (opens with a `<p>` then jumps to `<h2>`), unlike every other scene. Add a scene `h1` with the model name as nested `h2`. WCAG 1.3.1, RGAA 9.

**Confirm-before-delete (reuse an existing in-app pattern)**
- `ui/src/views/Library.tsx:487-492,536-543` — "Remove"/"Discard" delete a multi-GB model file on a single click/Enter, no confirmation. Reuse the disk-slow-confirm inline pattern already in the same file (`:494-522`) rather than building a new mechanism. WCAG 3.3.4, RGAA 11.
- `ui/src/views/SkillsView.tsx:61-66` — per-card "Delete" fires immediately with no confirm/undo. Same fix, smaller blast radius. WCAG 3.3.4, RGAA 11.

## Phase 1 — Keyboard & focus management

Requires actual interaction-logic changes (focus traps, focus restore, keyboard handlers). Do this after Phase 0 so the visual/labelling groundwork (focus-ring contrast, `useDialogFocus` reuse target) is already in place.

**Modal dialogs — apply one consistent contract**
Five dialogs share the same gap: no `role="dialog"`/`"alertdialog"`, no `aria-modal`, no `aria-labelledby`, no focus trap, no focus-in on open, no focus-restore on close, no Escape-to-close. `useDialogFocus` already exists (`ui/src/views/ChatView.tsx:586-592`, used by `ImageModal`/`DocPreviewPanel` at lines 605/655) — extract it into a shared hook/component and apply it everywhere below instead of five bespoke fixes:
- `ui/src/views/SettingsSheet.tsx:69-72,282-284` — no Escape handler exists in the file at all (its two `onKeyDown`s, lines 474/1038, are unrelated); focus can Tab straight through the 35%-opaque scrim into the app behind it. **Blocker.** WCAG 4.1.2, RGAA 7.
- `ui/src/views/SignInDialog.tsx:14-17` — zero keydown listeners in the file. **Blocker.** WCAG 4.1.2, RGAA 7.
- `ui/src/views/UpdateDialog.tsx:29-32` — mounts abruptly whenever `updateStatus` flips (`App.tsx:72`); keep the existing "can't dismiss while installing" guard (lines 24-27) but still trap/manage focus, and use `role="alertdialog"` since it interrupts unprompted. **Blocker.** WCAG 4.1.2, RGAA 7.
- `ui/src/views/SkillsView.tsx:75-132` — same gaps for the New/Edit Skill modal; skill-card grid and "New Skill" button behind it stay fully tabbable/exposed while open. WCAG 4.1.2, RGAA 7.
- `ui/src/components/ToolConsentCard.tsx:17-31` — currently a plain `role="group"` div; this one *gates dangerous tool actions* per `dangerFlags`, so it needs `role="alertdialog"`/`aria-modal="true"` and focus moved in the moment `consent.toolId` becomes pending, restoring focus to the composer after. **Blocker** — of all five, this is the one where a missed keyboard user could be unable to either grant or deny a consequential action. WCAG 4.1.2, RGAA 7.

**ARIA menu widgets that promise keyboard interaction they don't implement**
`role="menu"`/`"menuitem"` commits to Up/Down/Home/End roving-focus; only Tab + `useCloseOnOutside` (`ui/src/components/Dropdown.tsx:6-27`, Escape/outside-click only) is wired up in all three cases. The skill-suggest listbox (`ChatView.tsx` `role="listbox"`, ~1559-1601, driven by the shared textarea's `onKeyDown` at line 1587) is a working example already in the codebase — either port that pattern or drop the roles to match actual Tab-only behavior:
- `ui/src/views/Sidebar.tsx:146-244` — "More" menu (trigger `aria-haspopup="menu"` at 151; `role="menu"`/`"menuitem"` at 157/160/172/185/197/210/223/235). WCAG 4.1.2, RGAA 7.
- `ui/src/views/ChatView.tsx:1155-1204` — model quick-picker menu. WCAG 4.1.2, RGAA 12.
- `ui/src/views/ChatView.tsx:1474-1556` — composer "+" add menu. WCAG 4.1.2, RGAA 12.

**Focus not returned/moved on state change**
- `ui/src/views/Sidebar.tsx:96-97,156-244` — selecting a "More" menu item (or Escape/outside-click) unmounts the `role="menu"` panel; focus silently reverts to `<body>` instead of the trigger button. Fix alongside the menu-widget item above (same file/PR). WCAG — RGAA 7.
- `ui/src/views/Onboarding.tsx:19,71-313` — scene transitions fully unmount/remount each `<section>` with focus never moved (no `useRef`/`.focus()`/`tabIndex` anywhere in the file). Add a ref + `.focus()` in a `useEffect` keyed on `scene`, with `tabIndex="-1"` on each scene's `h1`. RGAA 7.

## Phase 2 — Dynamic content & AT announcements

Live regions for content that already updates asynchronously — no new UI, but needs care around throttling so streaming text isn't read token-by-token.

**Chat & streaming**
- `ui/src/views/ChatView.tsx:884-1054` (messages container), `AssistantBody` at 425-433 — streamed replies and "thinking"/"searching" indicators have no live region anywhere. Wrap the streaming assistant message in a polite live region (`role="log" aria-live="polite" aria-relevant="additions text"`), throttled to avoid reading every token. **Blocker.** WCAG 4.1.3, RGAA 7.

**Model/engine status**
- `ui/src/views/Sidebar.tsx:254-345` — `EnginePill` (Ready/Warming up/Needs attention/Asleep) is a plain button with no live region despite `App.tsx` polling every 2s; `AutoUnloadTooltip.tsx:102` already uses the correct `role="status"` pattern elsewhere in the same app. WCAG 4.1.3, RGAA 7.

**Onboarding**
- `ui/src/views/Onboarding.tsx:216-278` — download progress text (241-248), download-failed (216-226), and engine-failed "needs a nudge" (260-267) branches all update silently. Wrap progress in throttled `role="status" aria-live="polite"`, failures in `role="alert"`. **Blocker.** WCAG 4.1.3, RGAA 7.

**Library**
- `ui/src/views/Library.tsx:428-441` — download progress bar is a bare div, no `role="progressbar"`/`aria-valuenow`. WCAG 4.1.2, RGAA 7.

**Discover**
- `ui/src/views/Discover.tsx:183-194,376-379,384-393,447-449,464-469` — search "Looking…"/no-results text, details/readme errors, the "Added" confirmation that silently replaces the Add button, and the addError/"See in Library" button all mutate the DOM with no `aria-live`/`role`. Same pattern exists in `Library.tsx:425,494-522` (download error text, disk-slow-confirm panel). WCAG 4.1.3, RGAA 7.

**Plugins**
- `ui/src/views/Plugins.tsx:107-123` — setup progress bar, no `role="progressbar"`. WCAG 4.1.2, RGAA 7.
- `ui/src/views/Plugins.tsx:125-141` — `setupFailed`/`serviceFailed` blocks have no `role="alert"`/live region. WCAG 4.1.3, RGAA 7.

**Shared progress-indicator components**
- `ui/src/components/ProgressRing.tsx:14-19` — `role="img"` with a static one-time `aria-label` snapshot instead of `role="progressbar"` + `aria-valuenow`; used in Onboarding's download scene. WCAG 4.1.2, RGAA 7.
- `ui/src/views/UpdateDialog.tsx:46-57` — install-progress bar has zero ARIA (adjacent text at 59-67 is a partial equivalent but not live). WCAG 4.1.2, RGAA 7.
- Recommend factoring these two (plus Library's and Plugins' bars above) into one shared `<ProgressBar role="progressbar">` primitive so the semantics are implemented and tested once instead of four times.

**Remaining dialog status/error text**
- `ui/src/views/SignInDialog.tsx:18-19` (error paragraph), `ui/src/views/UpdateDialog.tsx:59-67,70` (byte/speed/ETA readout, error text), `ui/src/views/SettingsSheet.tsx:888` (diagnostics status line) — none wrapped in a live region. WCAG 4.1.3, RGAA 7.

## Phase 3 — Structural & content

Bigger-shape changes: touches the persistent app shell and the core chat message list, best scheduled after Phase 2's live-region work on the same containers so they're designed together rather than reworked twice.

**Landmark structure of the app shell**
- `ui/src/views/Sidebar.tsx:100` — the primary-navigation sidebar (new chat, conversation history, view switcher) is wrapped in `<aside>` (exposes as "complementary" — the opposite of its actual role). Nest a `<nav aria-label="...">` around new-chat/conversation-list/view-switcher (the existing inner `<nav className="conv-list">` only covers the conversation list). `App.tsx:50` — the custom titlebar carrying the only app-name text (`.sidebar-brand`, `Sidebar.tsx:102`) has no `<header>`/banner landmark, and there's no `h1` anywhere in the persistent shell chrome (confirmed via project-wide grep — `h1` only appears inside per-view content). Consider giving the app name an `h1`. WCAG 1.3.1, RGAA 9.

**Bypass blocks / skip navigation**
- `ui/src/App.tsx:48-76` — no skip-link anywhere in the app; in the default chat view, Tab actually starts on `ChatToolbar`'s settings-toggle icon-button (titlebar precedes Sidebar in the DOM), then into the sidebar's long conversation list with two focus stops per row. Add a visually-hidden "skip to conversation" link as the first focusable element, revealed on focus, moving focus to the main pane. WCAG 2.4.1, RGAA 12.

**Chat message semantics**
- `ui/src/views/ChatView.tsx:884,953,996` — the message list and each user/assistant turn are plain `<div>`s with no `role="list"`/`"listitem"` structure and no visually-hidden sender label (sender identity is CSS-only). Wrap turns in a `role="list"` container with `role="listitem"` rows plus a visually-hidden "You said"/"Assistant said" label per turn. This should land in the same body of work as Phase 2's streaming live-region change since both touch the same container. WCAG 1.3.1, RGAA 9.

## Tooling & regression prevention

Sized for the team as it exists today — reuse the CI job that's already there rather than adding new workflow files.

- **ESLint + `eslint-plugin-jsx-a11y`.** `ui/` currently has no ESLint config or dependency at all (verified: no `.eslintrc*`/`eslint.config*`, no `eslint` entry in `ui/package.json`). Add a minimal flat config with the `jsx-a11y` recommended rule set, and run it as a new step in the existing `frontend (typecheck, unit, mocked e2e)` job in `.github/workflows/ci.yml` — no new workflow file needed.
- **`@axe-core/playwright` in the existing e2e suite.** Playwright is already set up (`ui/playwright.config.ts`, `ui/tests/e2e/onboarding.spec.ts`) and already runs in CI via `npm run test:e2e` in that same `ci.yml` job. Add `@axe-core/playwright` as a dev dependency and an `expect(results.violations).toEqual([])` assertion in the existing spec(s) (onboarding first, then chat/library/discover as they're touched) rather than standing up a separate a11y test project.
- **Contrast-token regression test.** Since `--ink-faint`/`--warn`/`--ok` are now reused across ~10 files after Phase 0, add one small plain-JS unit test that computes WCAG relative-luminance contrast for the CSS custom-property pairs actually used for text/icons and asserts the 4.5:1/3:1 floors — run it as a normal `npm run` step in the same CI job, not a new pipeline, so a future token edit can't silently regress every screen at once.
- **One manual screen-reader smoke pass per release.** Automated tools (axe, jsx-a11y) won't catch live-region timing or focus-order regressions on the five now-fixed dialogs, the chat-streaming live region, or onboarding's scene-focus handling — a short VoiceOver pass (matching the shipped desktop platform) before each release covers what static analysis can't.
- **Deliberately not doing:** a dedicated accessibility CI workflow file, a Storybook a11y addon, or per-component visual-regression screenshots — the existing single frontend CI job is the right scope for a team this size; add to it, don't parallel it.

## Out of scope for this plan

- **WCAG 2.4.13 Focus Appearance (AAA)** — cited on two findings (`plugins.css:152-154`, `global.css:267-283`) as the closest-matching SC; the Phase 0 fix already clears the AA-level 1.4.11 Non-text Contrast bar, so chasing full AAA elsewhere in the app is not a goal of this plan.
- **RTL / additional-locale accessibility** — no finding in this audit touched translated content directly; a separate pass is warranted whenever OpenPal ships additional locales, not now.
- **Full native screen-reader compatibility matrix (VoiceOver/NVDA/JAWS × OS versions)** — one manual VoiceOver smoke pass per release (Tooling section) is proportionate for a small team; a full AT compatibility matrix can follow once Phases 0-2 are live and stable.
- **Swapping hand-rolled dialogs/menus for a headless-UI library (Radix, Ark, etc.)** — the Phase 1 fixes intentionally reuse and extract the app's own `useDialogFocus`/`useCloseOnOutside` hooks so a new dependency isn't required; worth reconsidering only if the number of hand-rolled interactive widgets keeps growing.
- **Routing accessibility fixes through a second/specialized model** (e.g., an LLM-generated alt-text or ARIA-label service) — not applicable here regardless of OpenPal's mono-model-first principle, since every fix in this plan is static markup, CSS, or focus-management code, not something needing model inference at all.
