# Accessibility audit — OpenPal UI

Scope: the whole `ui/` React app (App shell, Chat, Onboarding, Library, Discover, Plugins,
Skills, and every dialog/overlay), evaluated against **WCAG 2.2 Level AA** and **RGAA 4.1** (the
French accessibility framework's 13 thematic groups). Out of scope: native OS-level chrome
outside the Tauri webview (window controls, system menu), which is provided by the OS, not by
this codebase.

## Methodology

This audit was produced by independent review passes over six areas of the UI (App shell &
navigation, Chat & streaming, Onboarding, Library & Discover, Plugins & Skills, and Dialogs /
dropdowns / overlays), each covering its own files in full. Every candidate finding was then
re-verified by a second, independent pass that re-read the actual source and either confirmed it,
corrected inaccurate details (a line number, a WCAG/RGAA citation), or dropped it if it didn't
hold up. Of 57 candidate findings, 0 were dropped; 7 had a citation or line-range correction
applied (already reflected below — this document reports the corrected values, not the original
guesses). Two issues (menu-role keyboard navigation on the Sidebar's "More" menu, and on
ChatView's two composer menus) were independently flagged by two different review passes; they're
listed once below with a note that they were corroborated rather than as duplicates.

There is currently no automated accessibility tooling in this repo (no `eslint-plugin-jsx-a11y`,
no axe integration in the Playwright e2e suite, no ESLint at all) — see the "Tooling" section of
[ACCESSIBILITY_PLAN.md](ACCESSIBILITY_PLAN.md) for a proposal sized to this project.

## Summary

| Severity | Count | Meaning |
|---|---|---|
| **Blocker** | 7 | Blocks a core flow for keyboard/AT users (can't complete onboarding, can't read a streamed reply, can't safely respond to a dangerous tool-consent prompt, can't operate a modal dialog). |
| **Major** | 29 | A real WCAG 2.2 AA / RGAA failure that degrades or breaks a specific interaction, but doesn't block the whole flow. |
| **Minor** | 21 | A real conformance gap with a narrower or lower-frequency impact (e.g. missing `aria-controls` wiring, a single non-essential animation). |
| **Total** | 57 | |

Findings by area: App shell & navigation (9), Chat & streaming (12), Onboarding (6), Library &
Discover (11), Plugins & Skills (9), Dialogs/dropdowns & overlays (10, two corroborating earlier
findings rather than net-new).

**The 7 blockers**, because they're what makes this "broken" rather than "needs polish" for a
keyboard or screen-reader user:

1. Streamed chat replies are never announced to screen readers (no live region at all) — [C1](#c1)
2. The tool-consent prompt that gates potentially dangerous actions (shell exec, raw device I/O,
   sudo) has no dialog semantics and never receives keyboard focus — [C2](#c2)
3. Onboarding's model-download step (progress, failure, and "engine needs a nudge" states) is
   completely silent to assistive tech — [O2](#o2)
4. The model fit-status badge ("will this run on my machine?") fails contrast in three of its
   five states — [L1](#l1)
5–7. Settings, Sign In, and Update are all plain unstyled-semantics `<div>` pairs with no
   `role="dialog"`, no focus trap, no focus-in/out management, and no Escape handling —
   [D1](#d1), [D2](#d2), [D3](#d3)

## Cross-cutting patterns

Most of the 57 findings are instances of a small number of root causes. Fixing the root cause
once (a token, a shared hook) resolves many findings at once — see
[ACCESSIBILITY_PLAN.md](ACCESSIBILITY_PLAN.md) for how these map to remediation phases.

### 1. `--ink-faint` is used for real body text, but fails contrast

`--ink-faint` (`#a29685` light / `#786d5e` dark, `ui/src/styles/global.css:13,73`) computes to
roughly **2.4–3.6:1** against the paper backgrounds it's painted on, well under the 4.5:1 floor
for normal text (and under 3:1 for the icon use). It's used as if it were a safe "secondary text"
color across five of the six audited areas: conversation timestamps and the app version string
(shell), badge text on every model card (library/discover), onboarding's download-reassurance and
hardware-scan labels, plugin/skill empty-state and meta text, and Settings' field hints. `--ink-soft`
(`#6d6356`, ~5.5–6.9:1) already exists in the same token file and passes. See A5, O1, L1, L2, P8,
D6 in the tables below for every use site.

### 2. `role="menu"`/`"menuitem"` used without the keyboard interaction they promise

Three menus — the Sidebar's "More" navigation menu, ChatView's model quick-picker, and ChatView's
composer "+" menu — use ARIA menu roles, which commit assistive tech (notably JAWS/NVDA's menu
navigation mode) to expecting Up/Down/Home/End roving focus. None of the three implement it; all
three only support Tab and Escape (via the shared `useCloseOnOutside` hook,
`ui/src/components/Dropdown.tsx`). The codebase already has a correct example to copy: the
skill-suggest popup in ChatView uses `role="listbox"`/`"option"` with real arrow-key handling.
See A2/D4 and C7/D5.

### 3. Low-contrast, low-opacity focus rings (`--accent-soft`)

The Discover card grid, the Plugins enable/disable toggle, and the global `text`/`textarea` input
style (which drives the Skills editor fields) all use `box-shadow: 0 0 0 3px var(--accent-soft)`
as their **only** focus indicator. `--accent-soft` is the accent color at 9–13% alpha; composited
over the surrounding backgrounds it measures **~1.1–1.6:1** — nowhere near the 3:1 WCAG 2.2 Focus
Appearance / Non-text Contrast floor. For the Discover grid specifically, this is the *only*
keyboard-navigation cue for the entire screen. See L8, P4, P6.

### 4. No `prefers-reduced-motion` support anywhere in the app

A project-wide search of every stylesheet under `ui/src/styles/` found zero uses of
`prefers-reduced-motion`. Several looping/infinite animations run regardless of the OS-level
reduce-motion preference: chat's typing caret, thinking-dot, searching-spinner, and
recording-pulse; onboarding's "waking up the model" breathe animation; the plugin setup
indeterminate sweep; and lower-amplitude width/border-color transitions on progress bars and
Discover cards. See A6, C10, O6, L11, P9.

### 5. Modal dialogs and overlays with no dialog semantics or focus management

Five separate surfaces — Settings, Sign In, Update, the Skills new/edit modal, and the
tool-consent card — are each a hand-rolled `<div>` pair with **no** `role="dialog"`/`"alertdialog"`,
**no** `aria-modal`, **no** `aria-labelledby`, **no** focus moved in on open, **no** focus trap,
**no** focus restored on close, and (except where a separate Escape/outside-click hook happens to
be wired up) **no** Escape handling. The codebase already has the fix half-built: `useDialogFocus`
(`ui/src/views/ChatView.tsx:586-592`) is used correctly by `ImageModal` and `DocPreviewPanel` in
the same file — it just isn't reused for these five. See D1, D2, D3, P5, C2.

### 6. Async status, progress, and errors update the DOM with no live region

Streaming chat replies, onboarding's download progress/failure, Library's and Plugins' download
bars, Discover's search/add/error states, and the three dialogs' status/error text all mutate
visible text or a progress bar with no `aria-live`, `role="status"`, or `role="alert"` anywhere.
A screen-reader user who isn't focus-parked on that exact DOM node gets no feedback that anything
happened. See C1, O2, L5, L6, P2, P3, D7, D8, D9.

## Detailed findings by area

Each entry: **severity** · file:lines · WCAG 2.2 SC (where applicable) · RGAA 4.1 topic — issue,
then the fix.

### App shell & navigation

<a id="a1"></a>**A1 · Major** — `ui/src/styles/chat.css:117-137` (button in `Sidebar.tsx:131-138`)
· WCAG 2.4.7 · RGAA 12 (Navigation)
The per-conversation delete button is `opacity: 0`, revealed only on `:hover` — no `:focus-visible`
or `:focus-within` rule, so a keyboard user tabs onto an invisible destructive control. The same
file already has the right pattern (`.msg-actions:focus-within`, line 513) — mirror it.

<a id="a2"></a>**A2 · Major** — `ui/src/views/Sidebar.tsx:146-244` · WCAG 4.1.2 · RGAA 7 (Scripts)
The "More" navigation menu (Models/Browse/Plugins/Skills/Account/Settings) uses
`role="menu"`/`"menuitem"` with only Tab-based keyboard support — see [pattern 2](#2-rolemenumenuitem-used-without-the-keyboard-interaction-they-promise).
Independently corroborated by the overlays review pass (D4).

**A3 · Minor** — `ui/src/views/Sidebar.tsx:116-121, 161-205` (CSS at `chat.css:323-326`) ·
WCAG 4.1.2 · RGAA 12 (Navigation)
The active conversation and active view menu item are indicated by CSS class only —
no `aria-current` on either button.

**A4 · Major** — `ui/src/views/Sidebar.tsx:96-97, 156-244` · RGAA 7 (Scripts)
Closing the "More" menu (by selection, Escape, or outside click) unmounts the menu panel without
returning focus to the trigger button; focus silently reverts to `<body>`.

<a id="a5"></a>**A5 · Major** — `ui/src/styles/global.css:13,73` · WCAG 1.4.3 · RGAA 3 (Colors)
`--ink-faint` fails contrast — see [pattern 1](#1---ink-faint-is-used-for-real-body-text-but-fails-contrast).
In-scope use sites: `.conv-when` (chat.css:114), `.app-version` (chat.css:318), idle `.icon-btn`
(chat.css:1187).

**A6 · Minor** — `ui/src/styles/global.css:370-393` · RGAA 13 (Consultation)
No `prefers-reduced-motion` query anywhere in the project — see [pattern 4](#4-no-prefers-reduced-motion-support-anywhere-in-the-app).

**A7 · Major** — `ui/src/views/Sidebar.tsx:254-345` · WCAG 4.1.3 · RGAA 7 (Scripts)
`EnginePill` (Ready/Warming up/Needs attention/Asleep), polled every 2s by `App.tsx`, has no
`role="status"`/`aria-live` — state transitions, including the failure state, are never announced.
`AutoUnloadTooltip.tsx:102` already shows the correct pattern in the same app.

**A8 · Minor** — `ui/src/views/Sidebar.tsx:100` · WCAG 1.3.1 · RGAA 9 (Structuration)
The sidebar (primary navigation) is wrapped in `<aside>`, which exposes as a "complementary"
landmark — the opposite of its actual role. The custom titlebar also has no `<header>`/banner
landmark, and there's no `h1` anywhere in the persistent shell chrome.

**A9 · Minor** — `ui/src/App.tsx:48-76` · WCAG 2.4.1 · RGAA 12 (Navigation)
No skip-link anywhere in the app. In the default chat view, Tab starts on ChatToolbar's settings
toggle (titlebar precedes Sidebar in the DOM), then a long conversation list with two focus stops
per row, before reaching the main content.

### Chat & streaming

<a id="c1"></a>**C1 · Blocker** — `ui/src/views/ChatView.tsx:884-1054` (messages container),
`AssistantBody` 425-433 · WCAG 4.1.3 · RGAA 7 (Scripts)
Streamed assistant replies and the "thinking"/"searching" indicators render into plain `<div>`s
with no `aria-live`/`role="log"`/`role="status"` anywhere. A screen-reader user gets zero
notification that a reply has started, is progressing, or finished.

<a id="c2"></a>**C2 · Blocker** — `ui/src/components/ToolConsentCard.tsx:17-31` · WCAG 4.1.2 ·
RGAA 7 (Scripts)
The tool-consent prompt (Allow once / Always allow / Deny), which gates potentially dangerous
actions (fork-bomb shape, raw device I/O, sudo/priv-esc per `dangerFlags`), is a plain
`role="group"` div. Unlike `ImageModal`/`DocPreviewPanel` in the same file, it never calls
`useDialogFocus` — nothing moves keyboard focus to it when it appears.

**C3 · Major** — `ui/src/views/ChatView.tsx:1580-1619` · WCAG 3.3.2 · RGAA 11 (Forms)
The composer `<textarea>` has no persistent `aria-label` — only a `placeholder` that changes with
engine state and disappears once the user types.

**C4 · Major** — `ui/src/views/ChatView.tsx:895-917` · WCAG 3.3.2 · RGAA 11 (Forms)
The inline "edit a past message" `<textarea>` has no accessible name at all.

**C5 · Major** — `ui/src/views/ChatView.tsx:884, 953, 996` · WCAG 1.3.1 · RGAA 9 (Structuration)
The message list and each turn are plain divs with no `role="list"`/`"listitem"` structure and no
visually-hidden sender label — sender identity is CSS-only.

**C6 · Major** — `ui/src/components/Markdown.tsx:81-113` (CodeBlock), `:141-145` (TableWrap);
`chat.css:1118-1123`, `:1046-1048` · WCAG 2.1.1
Both the code-block `<pre>` and the wide-table wrapper scroll horizontally via `overflow-x: auto`
but aren't keyboard-focusable (no `tabIndex`), so a keyboard-only user can't scroll them.

**C7 · Minor** — `ui/src/views/ChatView.tsx:1155-1204` (model quick-picker), `1474-1556` (add menu)
· WCAG 4.1.2 · RGAA 12 (Navigation)
Same `role="menu"` pattern as [A2](#a2) — see [pattern 2](#2-rolemenumenuitem-used-without-the-keyboard-interaction-they-promise).
Independently corroborated by the overlays review pass (D5).

**C8 · Major** — `ui/src/views/ChatView.tsx:1053` · WCAG 4.1.3 · RGAA 7 (Scripts)
The turn-level `chat-error` div has no `role="alert"`/`aria-live` — failures are silent to screen
readers.

**C9 · Minor** — `ui/src/views/ChatView.tsx:1090` · WCAG 4.1.3 · RGAA 7 (Scripts)
`EngineBanner`'s failed-engine-load banner uses `role="status"` (polite); it should be
`role="alert"` given it's a failure state. `AutoUnloadTooltip.tsx:102` already uses `status`
correctly for its genuinely non-urgent banner.

**C10 · Minor** — `ui/src/styles/chat.css:579-587` (`.caret`), `595-601` (`.thinking-dot`),
`684-696` (`.searching-spinner`), `853-860` (`.trace-live`), `1280-1290` (`.composer-tool.is-recording`)
· WCAG 2.2.2 · RGAA 13 (Consultation)
Five infinite animations run for the duration of every generation with no
`prefers-reduced-motion` override — see [pattern 4](#4-no-prefers-reduced-motion-support-anywhere-in-the-app).

**C11 · Minor** — `ui/src/styles/chat.css:1231-1233, 1340-1342` · WCAG 2.4.7 · RGAA 10 (Presentation)
`.composer textarea:focus { box-shadow: none; }` strips the focus ring, leaving only a
border-color change on `.composer:focus-within` as the sole visible focus indicator — verify its
contrast/thickness.

**C12 · Minor** — `ui/src/views/ChatView.tsx:264-279` (ThinkingPanel), `310-347` (ToolActivity),
`365-393` (AgentActivity) · WCAG 1.3.1 · RGAA 7 (Scripts)
All three disclosure toggles set `aria-expanded` but never `aria-controls`, and their disclosed
regions have no `id`.

### Onboarding

<a id="o1"></a>**O1 · Major** — `ui/src/styles/onboarding.css:90-93, 95-104, 131-137` ·
WCAG 1.4.3 · RGAA 3 (Colors)
`.ob-reassure`, `.ob-update-choice`, `.ob-stat-label` use `--ink-faint` — see [pattern 1](#1---ink-faint-is-used-for-real-body-text-but-fails-contrast).
Affects the download reassurance line, hardware-scan stat labels, and the ready-scene
auto-update-check label — exactly the copy a beginner reads to understand what's happening.

<a id="o2"></a>**O2 · Blocker** — `ui/src/views/Onboarding.tsx:216-278` · WCAG 4.1.3 · RGAA 7 (Scripts)
The download/setup step — progress text (bytes/speed/ETA), download-failure, and
engine-failed-nudge branches — has zero `aria-live`/`role` anywhere. This is the first-run
download step; a screen-reader user gets no feedback that anything is happening, succeeding, or
failing.

**O3 · Major** — `ui/src/views/Onboarding.tsx:165-172, 176-206` · WCAG 4.1.2 · RGAA 7 (Scripts)
The "Pick a different model" toggle has no `aria-expanded`, and the panel it shows/hides has no
`id`/`aria-controls` link to it.

**O4 · Minor** — `ui/src/views/Onboarding.tsx:139-151` · WCAG 1.3.1 · RGAA 9 (Structuration)
The "recommend" scene has no `h1` (opens with a plain `<p>`, jumps straight to `<h2>`), unlike
every other onboarding scene.

**O5 · Major** — `ui/src/views/Onboarding.tsx:19, 71-313` · RGAA 7 (Scripts)
Scene transitions fully unmount/remount each `<section>` with focus never programmatically moved
(no `useRef`/`.focus()`/`tabIndex` anywhere in the file) — keyboard/AT users don't land on the new
step's content.

**O6 · Minor** — `ui/src/styles/onboarding.css:273-277` · WCAG 2.2.2 · RGAA 13 (Consultation)
`.ob-waking`'s infinite `breathe` animation has no `prefers-reduced-motion` override — see
[pattern 4](#4-no-prefers-reduced-motion-support-anywhere-in-the-app).

### Library & Discover

<a id="l1"></a>**L1 · Blocker** — `ui/src/styles/global.css:309-314` (used in `Library.tsx:386`,
`Discover.tsx:228-230,435-437`) · WCAG 1.4.3 · RGAA 3 (Colors)
The model fit-status badge — "will this run on my machine," the single most important status on
every model card — fails 4.5:1 in 3 of 5 states at its actual 12px/600-weight size: tight/diskSlow
(~3.76:1), perfect/good (~4.18:1), tooBig (~2.44:1).

<a id="l2"></a>**L2 · Major** — `ui/src/styles/library.css:125-128, 142-146`;
`ui/src/styles/discover.css:129-132, 142-146, 200-203` · WCAG 1.4.3 · RGAA 3 (Colors)
`--ink-faint` used for publisher name, model size, the "why this model" rationale, repo owner, and
the fit-estimate caveat — see [pattern 1](#1---ink-faint-is-used-for-real-body-text-but-fails-contrast).

**L3 · Major** — `ui/src/views/Discover.tsx:156-172` · WCAG 3.3.2 · RGAA 11 (Forms)
The model search input has only a `placeholder` (no label); the sort `<select>` has no label at
all.

**L4 · Major** — `ui/src/views/Library.tsx:487-492, 536-543, 468-477` · WCAG 3.3.4 · RGAA 11 (Forms)
"Remove"/"Discard" delete an installed or partially-downloaded (multi-GB) model on a single
click/Enter with no confirmation step, undo, or "are you sure" — the app already has an inline
confirm pattern for a different action in the same file (494-522) to reuse.

**L5 · Major** — `ui/src/views/Discover.tsx:183-194, 376-379, 384-393, 447-449, 464-469` ·
WCAG 4.1.3 · RGAA 7 (Scripts)
None of the dynamically-appearing status text (search "Looking…"/no-results, details/readme
errors, the "Added" confirmation, the add-error) uses `aria-live`/`role`. Same pattern in
`Library.tsx:425, 494-522`.

<a id="l6"></a>**L6 · Major** — `ui/src/views/Library.tsx:428-441` · WCAG 4.1.2 · RGAA 7 (Scripts)
The download progress bar is a plain styled `<div>` — no `role="progressbar"`,
`aria-valuenow`/`min`/`max`.

**L7 · Major** — `ui/src/views/Library.tsx:450-455, 480-492, 536-543` · WCAG 2.4.6 · RGAA 11 (Forms)
"Cancel"/"Remove"/"Discard" repeat with the exact same accessible name across every card in the
grid, with nothing tying the name to which model it acts on.

<a id="l8"></a>**L8 · Major** — `ui/src/styles/discover.css:100-108` · WCAG 2.4.7 · RGAA 12 (Navigation)
`.discover-card:focus-visible`'s ring is ~1.13:1 contrast — see [pattern 3](#3-low-contrast-low-opacity-focus-rings---accent-soft).
This is the only keyboard cue for the entire Discover grid (cards are `role="button"`,
`tabIndex=0`).

**L9 · Minor** — `ui/src/views/Library.tsx:249-257` · WCAG 4.1.2 · RGAA 11 (Forms)
The model-picker `<input type="range">` has `aria-label` but no `aria-valuetext` — a screen reader
announces only a raw numeric index, never which model it corresponds to.

**L10 · Minor** — `ui/src/views/Library.tsx:75, 111, 142, 385` · WCAG 1.3.1 · RGAA 9 (Structuration)
Per-card model-name `<h2>` sits as a sibling of the section `<h2>` ("Other models"/"Cloud models")
instead of nested under it as `<h3>`.

<a id="l11"></a>**L11 · Minor** — `ui/src/styles/library.css:217-222`; `discover.css:97` ·
RGAA 10 (Presentation)
The download progress-bar width transition and Discover card border-color transition have no
`prefers-reduced-motion` override — see [pattern 4](#4-no-prefers-reduced-motion-support-anywhere-in-the-app).

### Plugins & Skills

**P1 · Major** — `ui/src/views/Plugins.tsx:77-90` · WCAG 4.1.2 · RGAA 11 (Forms)
The plugin enable/disable `<input type="checkbox">`'s only accessible name lives in a `title`
attribute on its empty-content wrapping `<label>`, which some browser/AT combinations don't
reliably expose as the name.

<a id="p2"></a>**P2 · Major** — `ui/src/views/Plugins.tsx:107-123` · WCAG 4.1.2 · RGAA 7 (Scripts)
The plugin-setup progress bar is two plain `<div>`s — no `role="progressbar"`,
`aria-valuenow`/`min`/`max`, no accessible name.

<a id="p3"></a>**P3 · Major** — `ui/src/views/Plugins.tsx:125-141` · WCAG 4.1.3 · RGAA 7 (Scripts)
`setupFailed`/`serviceFailed` blocks have no `role="alert"`/live region.

<a id="p4"></a>**P4 · Major** — `ui/src/styles/plugins.css:152-154` · WCAG 2.4.13 · RGAA 3 (Colors)
The plugin toggle's focus ring uses the same low-alpha `--accent-soft` as [L8](#l8) (~1.13:1) —
see [pattern 3](#3-low-contrast-low-opacity-focus-rings---accent-soft).

<a id="p5"></a>**P5 · Major** — `ui/src/views/SkillsView.tsx:75-132` · WCAG 4.1.2 · RGAA 7 (Scripts)
The New/Edit Skill modal has no `role="dialog"`/`aria-modal`/`aria-labelledby`, no focus trap or
`inert` on the page behind it, no Escape handler, and doesn't restore focus on close — see
[pattern 5](#5-modal-dialogs-and-overlays-with-no-dialog-semantics-or-focus-management).

<a id="p6"></a>**P6 · Major** — `ui/src/styles/global.css:267-283` · WCAG 2.4.13 · RGAA 3 (Colors)
The global `text`/`textarea` focus style (which drives the Skills editor fields) uses the same
low-contrast `--accent-line`/`--accent-soft` combination — see [pattern 3](#3-low-contrast-low-opacity-focus-rings---accent-soft).

**P7 · Minor** — `ui/src/views/SkillsView.tsx:61-66` · WCAG 3.3.4 · RGAA 11 (Forms)
The per-card "Delete" button destroys a user-authored skill immediately on click, with no
confirmation or undo.

<a id="p8"></a>**P8 · Major** — `ui/src/styles/plugins.css:43-50, 82-85`; `skills.css:45-52` ·
WCAG 1.4.3 · RGAA 3 (Colors)
`--ink-faint` used for empty-state and meta text — see [pattern 1](#1---ink-faint-is-used-for-real-body-text-but-fails-contrast).

<a id="p9"></a>**P9 · Minor** — `ui/src/styles/plugins.css:179-189` · RGAA 10 (Presentation)
The indeterminate setup-progress sweep animation has no `prefers-reduced-motion` override — see
[pattern 4](#4-no-prefers-reduced-motion-support-anywhere-in-the-app).

### Dialogs, dropdowns & overlays

<a id="d1"></a>**D1 · Blocker** — `ui/src/views/SettingsSheet.tsx:69-72, 282-284` · WCAG 4.1.2 ·
RGAA 7 (Scripts)
No `role="dialog"`/`aria-modal`/`aria-labelledby`; focus never moves in on open and there's no
trap (Tab can reach the app behind the translucent scrim); no Escape handler exists anywhere in
the file; focus isn't restored on close — see [pattern 5](#5-modal-dialogs-and-overlays-with-no-dialog-semantics-or-focus-management).

<a id="d2"></a>**D2 · Blocker** — `ui/src/views/SignInDialog.tsx:14-17` · WCAG 4.1.2 · RGAA 7 (Scripts)
Same structural gap as D1 — zero keydown listeners in the file at all.

<a id="d3"></a>**D3 · Blocker** — `ui/src/views/UpdateDialog.tsx:29-32` · WCAG 4.1.2 · RGAA 7 (Scripts)
Same gap again; also mounts abruptly whenever `updateStatus` flips away from `idle`
(`App.tsx:72`), interrupting the user unprompted — should be `role="alertdialog"` once fixed.
Keep the existing "can't dismiss while installing" guard.

<a id="d4"></a>**D4 · Major** — `ui/src/views/Sidebar.tsx:146-244` · WCAG 4.1.2 · RGAA 7 (Scripts)
Corroborates [A2](#a2) — same "More" menu, same missing keyboard interaction, found independently
by this review pass too.

<a id="d5"></a>**D5 · Minor** — `ui/src/views/ChatView.tsx:1155-1199, 1474-1554` · WCAG 4.1.2 ·
RGAA 12 (Navigation)
Corroborates [C7](#c7) — same two composer menus.

<a id="d6"></a>**D6 · Major** — `ui/src/styles/library.css:263-266` · WCAG 1.4.3 · RGAA 3 (Colors)
`.settings-hint` (used under nearly every field/toggle in Settings) and `.settings-value`
(`chat.css:1472-1476`, the numeric readout next to each generation slider) both use `--ink-faint`
— see [pattern 1](#1---ink-faint-is-used-for-real-body-text-but-fails-contrast).

<a id="d7"></a>**D7 · Minor** — `ui/src/views/SignInDialog.tsx:18-19` (error text);
`UpdateDialog.tsx:59-67, 70` (progress readout, error text); `SettingsSheet.tsx:888`
(diagnostics status) · WCAG 4.1.3 · RGAA 7 (Scripts)
None of these dynamic status/error nodes has a live region — see [pattern 6](#6-async-status-progress-and-errors-update-the-dom-with-no-live-region).

<a id="d8"></a>**D8 · Minor** — `ui/src/components/ProgressRing.tsx:14-19` · WCAG 4.1.2 · RGAA 7 (Scripts)
Progress is exposed as `role="img"` with a static one-time `aria-label` snapshot rather than
`role="progressbar"` + `aria-valuenow` — used in Onboarding's download scene, so updates aren't
announced as the download proceeds.

**D9 · Minor** — `ui/src/views/UpdateDialog.tsx:46-57` · WCAG 4.1.2 · RGAA 7 (Scripts)
The install-progress bar has zero ARIA semantics at all (no `role`, no `aria-value*`).

**D10 · Minor** — `ui/src/views/ChatToolbar.tsx:393-407` · WCAG 4.1.2 · RGAA 7 (Scripts)
The trigger declares `aria-haspopup="dialog"` but the popup it opens has no `role` at all — the
advertised widget type doesn't match what's rendered.

---

See [ACCESSIBILITY_PLAN.md](ACCESSIBILITY_PLAN.md) for the phased remediation plan built from
these findings.
