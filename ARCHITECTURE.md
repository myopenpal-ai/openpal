# OpenPal architecture

```
┌────────────────────────────────────────────────────────────────────┐
│  ui/  (React + TS, Vite · hand-written CSS + shadcn/ui Alert)      │
│  views: Onboarding · Chat · Library · Plugins · Settings           │
│  state: zustand store — single seam to Rust in src/lib/api.ts      │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ Tauri IPC: commands + Channels (streaming)
┌──────────────────────────────▼─────────────────────────────────────┐
│  crates/app  (openpal-app — Tauri shell)                           │
│  commands.rs: validation + friendly errors, nothing else           │
│  state.rs: AppState { paths, catalog, hardware, engine, … }        │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ plain Rust calls
┌──────────────────────────────▼─────────────────────────────────────┐
│  crates/core  (openpal-core — no GUI deps)                         │
│  hardware  catalog  custom  huggingface  selection  download       │
│  engine  plugin  settings  store                                   │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ spawns + HTTP (localhost only)
              ┌────────────────▼─┐        ┌──────────────────────────┐
              │  llama-server    │◄───────│  web-search plugin (Py)   │
              │  (llama.cpp)     │  HTTP  │  plugins/web-search/      │ ← PluginManager
              └──────────────────┘ tools  │  raw tool loop:           │   runs setup +
                staged by bundler/ into    │  search·fetch·rerank·cite │   per-turn chat
                crates/app/resources/llama │  trafilatura·bge-reranker │   (NDJSON stdio)
                                           └─────────────┬────────────┘
                                                         │ HTTP
                                            ┌────────────▼───────────┐
                                            │ SearXNG (managed sidecar)│ ← PluginManager
                                            │ fetched + run, no Docker │   service: spawn,
                                            └──────────────────────────┘   health, kill
```

`bundler/` (openpal-bundler) sits outside this stack entirely: a CLI used by
developers/CI to fetch the engine, generate icons, build, sign, and notarize.
The app never links against it.

## Design philosophy

OpenPal is opinionated by design, in service of one goal: get non-technical
people to actually adopt local-first LLM usage. The primary user is a total
beginner — someone who wants "my own AI on my Mac" without learning what a
quantization, a context window, or a system prompt is. Users who want to go
further are welcome, but they earn extra power through **Settings → Advanced**
and the **plugin system** — never by the default path growing more knobs.

**Mono-model first.** OpenPal loads exactly one model at a time, and that one
model handles every task — chat, the agent's tool use, web-search
summarization, all of it. This holds even where a second, specialized model
(a router, a reranker, an embedding model) would be measurably more efficient.
Juggling models is exactly the kind of decision a beginner shouldn't have to
make or even be aware of ("which model answered that?", "why is it
downloading something else now?"), and every extra model multiplies disk/RAM
budgeting, download management, and failure modes for a marginal quality
gain. When a feature seems to want a second model, prefer teaching the one
loaded model to do the job (better prompting, better tool design) before
reaching for another one — and if a second model is truly unavoidable, it
belongs behind an explicit opt-in (Advanced settings or a plugin), never the
default path.

**The one knowing exception: the prompt-injection detector.** Every desktop
install also downloads and runs a second, dedicated model: a small classifier
(`crates/core/src/injection/`, ~300 MB on disk, ~0.8–1 GB of RAM while it is
loaded) that watches text on its way to the chat model — web pages, files,
command output, attachments, other conversations, skills, the composer — for
instructions aimed at the AI, in the turns where that model has tools (agent
mode and web search). Plain chat, cloud chat and Writing Tools aren't
checked: with no tools, injected text there can't act without the user. When
it flags text the user didn't write, the
turn pauses before the model reads it and asks the user to Continue or Stop;
text the user typed is sent as written, with a notice in the reply. Both also
flag the agent's consent card and write a line in the app log. It is on the
default path and **has no off switch**, not even under Settings → Advanced.
That breaks both rules above, deliberately: the call was made for security in
issue #9, and the loaded model can't do this job anyway (an agent turn holds
llama-server's only inference slot for the whole turn, and cloud chat has no
local engine). The beginner-first cost is kept as small as it can be: a
review card only before flagged third-party text is read, plus a one-line
notice in the reply, a silent background download, and loading it only for
turns that use tools, then dropping it once it has sat idle for 5 minutes or
the chat model is unloaded. Design and limits:
`docs/AGENT_MODE.md` → "Prompt-injection detection". This is not a
precedent: the next feature that wants a second model starts from
mono-model first again.

**Unfinished work sits behind one switch.** Anything still being tested is
gated on Settings → Advanced → Experimental features
(`Settings.experimental_features`, off by default), so a beginner never meets
it. A plugin opts in with `"experimental": true` in its manifest
(`plugins/PLUGINS.md` §1); any other feature checks the setting itself
(`experimentalFeaturesOn` in `ui/src/state/store.ts`, the field directly in
Rust) and, if it has its own on/off choice, is named in the confirmation
shown when the switch is turned off. Turning the switch off never rewrites
a feature's own choice, and a feature graduates by dropping its gate.

## Key decisions

**llama.cpp as a managed subprocess, not FFI.** The core spawns the official
prebuilt `llama-server` and talks OpenAI-compatible HTTP on `127.0.0.1:<random
port>`. This avoids native build complexity, lets the bundler swap engine
versions without recompiling, and crashes in the engine can't take down the
app. The child is spawned with `kill_on_drop` and stopped on `RunEvent::Exit`,
so it never outlives the app.

**Core is Tauri-free.** `openpal-core` compiles without any GUI dependency, so
it can back a CLI or tests directly. The Tauri layer (`crates/app/commands.rs`)
only validates input, calls core, and translates errors into sentences a
beginner can act on.

**Curated catalog over model search.** Beginners don't want a HuggingFace
browser. `crates/core/data/catalog.json` is a hand-curated, embedded list of
13 verified models (URL + exact byte size checked against the host). Selection
(`selection.rs`) estimates each model's working set
(`weights × 1.10 + KV-cache + 700 MB`) against a budget of 70% of RAM and
ranks by fit, then hand-curated quality score. All copy it produces is written
for non-technical users.

**Custom models are an escape hatch, not a browser.** Settings → AI model
lets a user add any Hugging Face GGUF (paste a link / "owner/repo";
`huggingface.rs` lists the repo's single-file GGUFs via the public tree API —
the webview has no network, so Rust does it) or register a local `.gguf`
used in place (never copied; "Remove" only unlists it). `custom.rs` persists
them as plain `ModelSpec`s in `app.sqlite`'s `custom_models` table
(`source: huggingFace | localFile`), so status/fit/download/engine treat them
exactly like catalog entries. HF download sizes + sha256 come from the repo's
LFS metadata (`lfs.oid` *is* the content sha256 — verified) and are re-fetched
server-side at add time, never trusted from the UI. `params_b` (→ fit copy)
is estimated from file size + quant label; custom models are never "our
pick", and the recommendation flow stays catalog-only.

**Streaming via Tauri Channels.** Downloads and chat tokens stream through
`tauri::ipc::Channel<T>` (adjacently-tagged serde enums mirrored 1:1 in
`ui/src/lib/types.ts`). The frontend never talks to llama-server directly —
CSP only allows the IPC origin for `connect-src`.

**The webview sends nothing to other sites, and not on the CSP's word
alone.** Model-written text — replies, artifact previews, Hugging Face model
cards — is the one place outside content enters the page, so
`ui/src/components/Markdown.tsx` holds its own line regardless of what the
CSP allows (and under Vite and the e2e suite there is no CSP at all): an
image never renders as an `<img>`, only as a placeholder with its alt text,
and a link never holds the model's address in a live `href`, so no engine
can prefetch, preconnect or navigate to it. An http(s) link opens in the
system browser through `open_link` only after the user confirms a dialog
showing its real host and full address (`LinkConfirmDialog.tsx`); every other
target is inert. Navigation itself is fenced by the `on_navigation` guard in
`crates/app/src/lib.rs`, which keeps the window on the app's own origin.
The CSP (`img-src 'self' data:`, `connect-src ipc:`) is the backstop behind
both.

**Hand-written CSS first; Tailwind and shadcn/ui only for vendored
components.** The UI's look is the tokenised stylesheets in `ui/src/styles/`;
`global.css` holds the palette (light, plus the dark copy written once pinned
by `Settings.theme` and once under the OS media query). shadcn/ui components
are copied into `ui/src/components/ui/` as source — today only `Alert`, for the
in-app alerts — and styled by Tailwind CSS v4, loaded through
`@tailwindcss/vite` without Tailwind's preflight reset and scanning only that
folder (`styles/tailwind.css`). Their colours are `@theme inline` aliases of the
existing palette tokens, so Light, Dark and System all follow the one palette
and no second set of dark rules exists. Everything Tailwind emits sits in a
cascade layer and the hand-written CSS does not, so the hand-written CSS always
wins; existing components are not migrated to it.

**Resumable everything.** Downloads stream to `<file>.part` with HTTP Range
resume; quitting mid-download resumes later. Engine status is polled, and the
UI auto-wakes the engine on launch so the app opens ready to chat.

**Alerts are decided in the UI; the OS mirror is delivered by Rust.** Four
events — a model finishes loading, a model is unloaded, a model download
finishes, an answer finishes — each raise one in-app alert (the store's
`appAlerts`, rendered by `AlertHost`) and, when Settings → General's "Desktop
notifications" switch is on and OpenPal is not the focused window, the same
words as an OS notification (`notify_native`; Rust re-checks the switch and,
except on Android where tao cannot read it, the window's focus). The "answer
ready" in-app alert is skipped when that conversation is open in the chat view
with nothing laid over it (not while the More dialog or conversation search
covers it); the native one is never skipped for that reason. Events are
detected by diffing, never by trusting one signal: engine load/unload comes
from state transitions (`ui/src/lib/engineEvents.ts`), so the repeated
2-second status polls cannot repeat an alert. Auto-unload is decided in Rust
(`idle.rs`) whether or not the webview is awake; it only emits a payload-less
`engine-status-changed` nudge so the UI re-reads and diffs sooner. The switch
is `Settings.desktop_notifications`, off by default — including for a settings
row saved before the field existed (struct-level `#[serde(default)]`).

Each way of delivering is a `Notifier` backend in `crates/app/src/notify/`,
behind one contract (permission, request, settings page, markup, post):

- a bundled macOS app: `UNUserNotificationCenter` (`macos.rs`), for the
  permission, the posts and the clicks. `usernoted` will not serve one bundle
  as both a legacy and a modern client (it logs "You can't mix modern clients
  with legacy clients" and drops every legacy post, even from later launches),
  so nothing posts for OpenPal's bundle id through the legacy center. A
  delegate on the center lets the banner show even while OpenPal is the active
  app (a minimized window is not a focused one, but macOS would otherwise
  swallow the banner) and hears the clicks.
- macOS outside a bundle (`tauri dev`), where the modern center aborts the
  process: the legacy center through `notify-rust`, posting as Terminal
  (`macos_dev.rs`). A development convenience: no permission, no clicks.
- Windows: WinRT toasts through `tauri-winrt-notification` (`windows.rs`).
  Windows only shows a toast from an unpackaged app whose app id (AUMID) it
  knows, and drops the rest without an error. OpenPal ships on Windows only as
  a portable zip, with no Start-menu shortcut to carry the AUMID, so it writes
  `HKCU\Software\Classes\AppUserModelId\com.myopenpal.openpal` (a display name)
  itself: the one host trace a portable build leaves, written only once the
  switch is on (by the request and before each post; reading the permission,
  as Settings does, only looks). It also reads the per-app switch in Windows
  Settings, so Windows can report "blocked" too.
- Linux: the notification server over D-Bus through `notify-rust`
  (`freedesktop.rs`), with the body's markup escaped.
- Android: `NotifyPlugin.kt` (`android.rs`): a channel, the POST_NOTIFICATIONS
  runtime permission (Android 13 and later; Tauri's permission cache is what
  tells "can still ask" from "denied for good"), the app's notification
  settings page, and taps.
- iOS: `unsupported.rs`, until it has a backend.

`notify::select` picks one at the start of `setup()` (the OS by `cfg`; whether
a macOS binary is inside an app bundle at runtime), and commands only ever see
the managed `Notifications`. `get_app_info`'s `nativeNotifications` says at
boot whether there is a backend at all; that, not `isAndroid`, is what the UI
leaves the switch, its onboarding step and native delivery out on (`isAndroid`
only picks the switch's wording for a phone).

The OS's answer is one of `granted`, `denied`, `prompt` (not asked yet) and
`unsupported`; only macOS, Windows and Android can say `denied`, and turning
the switch on is what asks (`request_notification_permission`, which raises
macOS's and Android's prompt). Because neither asks again after a refusal, the
"blocked" line tells the user what to switch on and carries an "Open
notification settings" button (`open_notification_settings`): macOS's
Notifications pane on OpenPal's own row via the bundle id, Windows' notifications
page, or Android's app notification settings; if the page cannot be opened the
line says where to look instead.

A click on a notification (a tap on Android) is reported by the backend to
`notify::Clicks`, which raises the window and sends a payload-less
`notification-clicked` nudge; the store then reads the click
(`take_notification_click`) and, for "answer ready", opens that conversation
with nothing laid over it. The store also reads once at the end of boot, so a
click that launched the app, or came in while it was locked, still lands. A
Windows toast reports a click only while it is on screen (a click from the
notification center would need a COM activator), and a Linux server only when
it supports actions.

**Per-model engine args.** Catalog entries carry an `extraArgs` field for
model-specific launch flags, currently unused (every curated entry ships
`[]`). Thinking suppression for hybrid "thinking" models — so beginners
never see chain-of-thought walls of text — is instead governed uniformly by
`Settings.show_thinking`: when off, `commands::start_engine` appends
`--reasoning-budget 0` to every model's launch args, taking effect on the
next model load.

**Plugins are runtime add-ons with managed setup + services.** `core/plugin.rs`
(`PluginManager`) discovers plugin folders — built-in (`plugins/<id>/` in dev,
`Resources/plugins/` packaged) and user-dropped (`<app-data>/plugins/<id>/`,
for community plugins later) — each described by a `plugin.json` manifest. A
plugin can be enabled/disabled (persisted in `app.sqlite`'s `plugin_records`
table), may need a
one-time **setup** (the manager makes a Python venv, `pip install`s its
requirements, and runs its setup script, streaming progress), may run a
long-lived **service** (a managed sidecar with the *exact* engine lifecycle:
free port → spawn `kill_on_drop` → health-poll → kill on disable/exit), and may
handle a **chat** turn. The plugin dir is read-only; all mutable state (venv,
downloads, logs) lives under `<app-data>/plugin-data/<id>/`. The full contract
(manifest, setup/service/chat process protocols, TS mirror) is
`plugins/PLUGINS.md`. Commands: `list_plugins`, `set_plugin_enabled`
(streams `PluginEvent`). This keeps Rust generic: per-plugin specifics live in
the plugin's own (Python) code. A manifest can set `"experimental": true` to
sit behind Settings → Advanced → Experimental features (`plugins/PLUGINS.md`
§1; every shipped add-on does), or `"core": true` to opt out of the
Add-ons/enable-toggle contract entirely (§9 there; no shipped plugin does
today).

**Web search is an experimental add-on, then a per-conversation toggle.** The
`web-search` plugin is off by default and only listed while Experimental
features is on. Turning it on in Plugins installs its local SearXNG; the
composer's globe then turns search on for one conversation at a time (off
again for each new or reopened one), the consent gesture for anything
leaving the machine. The search loop itself is native Rust
(not a Python sidecar): `chat_stream` (`commands.rs`) routes a turn with web
search on to `web_search_chat_turn`, which runs the same `run_agent_turn` loop
agent mode uses (`crates/core/src/agent/orchestrator.rs`), scoped to just the
`web_search`/`fetch_url` tools (`crates/core/src/agent/builtin/websearch.rs`)
— main-content extraction via the `readable-readability` crate, chunking +
BM25 reranking via `rerank.rs` (no cross-encoder, no second model — see
"mono-model first" above), emitting the same `ChatEvent`s agent-mode tool
calls do plus `SearchStarted`/`SearchResult`/`Fetching`/`Citation` for the
UI's inline `[N]` source markers. Full agent mode reaches the same tools
indirectly, through `spawn_subagent`'s `"web-research"` kind
(`crates/core/src/agent/builtin/subagent.rs`).

**Where the web tools may send requests.** Neither tool asks before it runs,
so the bounds are hard rules in Rust, identical in both surfaces:

- `web_search` talks only to the local SearXNG. Its client follows no
  redirect, and `!` operators are stripped from the query, so an external
  bang can't forward the query to another site.
- `fetch_url` opens only a URL that exactly matches one the user wrote
  (message, skill body, attachment, system prompt, a typed `ask_user`
  answer), one a search returned, or an earlier answer's citation.
  Anything else is refused before any DNS lookup.
- `fetch_url` is limited to 10 fetches per turn, helpers included.

Two residuals remain. Search queries still reach SearXNG's third-party
engines. And which result the model opens is a low-bandwidth channel an
injected page can steer. Details are in
[docs/AGENT_MODE.md](docs/AGENT_MODE.md#where-the-web-tools-may-send-requests).

`plugins/web-search/` ships `service.py` + `setup_plugin.py` and runs
through `PluginManager` (`core/plugin.rs`) with the same one-time Python
setup + managed-sidecar lifecycle every plugin gets: turning it on
(`set_enabled`) downloads a pinned SearXNG source tarball and `pip install`s
its deps into the venv — **SearXNG installs and runs itself, no Docker** —
then starts the service (health `GET /healthz`). (For a while it was a
`"core": true` plugin that self-provisioned on the first search, with no
enable step; it went back to being an add-on when Experimental features
arrived.)

A turn that searches calls
`PluginManager::ensure_capability_service("search", on_event)`, which finds
the active plugin providing `search` (enabled, set up, not gated off by
Experimental features) and starts its service if it isn't running,
streaming progress through the same `PluginEvent` shape Add-ons setup uses.
`PluginEvent::as_search_setup_chat_event` translates that into
`ChatEvent::SearchSetupProgress`, so the chat UI shows a status line instead
of an unexplained pause. `agent_bridge::build_web_search_deps` calls this
eagerly (a plain-chat turn with the toggle on is *definitely* about to
search); `SpawnSubagentTool` instead holds its own `Arc<PluginManager>` and
calls it lazily, only when its `"web-research"` kind is actually invoked —
so a full agent-mode turn that never touches web search never pays the
start-up cost. Either way the resolved base URL lands in `AgentDeps::search_service_url`
/ a freshly built `WebSearchTool`. The webview never reaches the service
directly (CSP forbids it); only Rust does. Cancellation reuses `chat_cancel`.
Packaging plugins + their venvs into the signed `.app` (PyInstaller /
inside-out signing) is deferred; in dev the manager builds venvs with
`python3` (override via `OPENPAL_PYTHON`) — though this plugin (no
`python.requirements`) needs no venv deps of its own, only SearXNG's.

**Agent tools are Rust-native, not a plugin.** `crates/core::agent`
(Tauri-free, like the rest of core) lets the model call tools — `run_shell`,
`run_code`, `read_file`, `write_file` — that act on the user's machine.
Unlike web search, its reasoning loop talks to `llama-server` directly
(parsing its own SSE stream for `delta.tool_calls`; `engine.rs`/`sse.rs` are
untouched) rather than through a Python sidecar, because every tool needs
OS-level sandboxing (macOS Seatbelt for `run_shell`/`run_code`) or
filesystem jailing (`read_file`/`write_file`) that can only live in Rust
per this app's own security boundary — and a Rust-native design ships in
the portable build for free, unlike every existing Python-venv plugin. Off
by default (`Settings.agentModeEnabled`); the workspace folder tools are
jailed to is system-managed, not user-chosen — one folder per conversation
under the app's data dir. Every tool call needs the user's explicit per-call
consent (Allow once / Always allow / Deny) regardless. Full design in
`docs/AGENT_MODE.md`.

**Request triage classifies against the running engine, not a second model
or a plugin — and it's entirely silent.** `EngineManager::classify`
(`engine.rs`) sends a tiny, non-streaming completion to the already-running
llama-server, output constrained via a GBNF grammar to one of `TaskType`'s
labels (general / coding / writing / mathScience / agentic) — no extra
process, no model swap, cheap enough to run on a debounce while the user
types. `TaskType` is the same enum the per-model, per-task sampling
recommendation feature introduced (`ModelSpec.task_defaults`,
`Catalog::recommended_sampling`) — triage just gives it a real, evolving
value instead of every conversation being permanently `General`. Its
**core** effect is applying that task's recommended sampling
(`recommendedSampling`) to the conversation, gated by one on-by-default
setting, `Settings.auto_pick_sampling`, implemented in `useTriageAutopilot`
(`ChatView.tsx`). It backs off automatically the moment the user hand-tunes
a slider (detected via the Generation panel's own `samplingParamsEqual`
"auto-suggested" comparison — no separate override flag needed). Nothing
about this is shown in the chat UI (no chip, no indicator); the only trace
is a `tracing::info!` log line (`classify`'s own match, plus
`commands::log_triage_action` when a preset is actually applied), visible
in Settings → Advanced → Logs & diagnostics. Disabling the setting doesn't hide anything (there's
nothing to hide) — it just leaves sampling exactly as it is without request
triage at all.

`recommendedSampling`/`Catalog::recommended_sampling` are also
thinking-mode-aware: a model with a hybrid think/non-think mode (today,
the Qwen3.5 and Qwen3.6 lines) can carry a second table,
`ModelSpec.task_defaults_thinking`, consulted ahead of the regular
`task_defaults` table whenever `Settings.show_thinking` is on — so the same
model/task gets different curated sampling depending on whether it's
actually showing its reasoning trace.

**Auto-picking the *model* itself is a separate, opt-in plugin, not core.**
`plugins/smart-model-router/` (Smart Model Router) is a python-less, pure
Rust+UI capability-flag plugin (`plugins/PLUGINS.md` §8, same shape as the
`hf-browser` Model Browser add-on) — enabling it from Add-ons is its only
on/off switch, no `Settings` field of its own involved (though its manifest
marks it experimental, so it's only reachable and active while Settings →
Advanced → Experimental features is on, see `plugins/PLUGINS.md` §1). When
enabled, `useTriageAutopilot` also ranks
already-*installed* models for the classified task
(`ModelSpec.task_scores`, `ui/src/lib/triage.ts`'s `bestModelForTag`) and
auto-switches via the existing `start_engine` path. Because a switch means
a full engine reload (180s normal, up to an hour for a disk-slow model —
see `engine.rs`), the trigger is deliberately conservative: an idle engine
switches immediately, but a hot one requires the same suggestion to stay
stable across two consecutive classify ticks (or, failing that, right
before the message is actually sent — a send-time backstop), and picking a
model by hand (`ModelQuickPicker`) pins auto-pick off for the rest of that
typing burst. The one place this plugin reaches into the core app (beyond
the UI reveal every capability-flag plugin gets) is
`commands::classify_message`'s gate, which via
`PluginManager::capability_enabled("smart-model-router")` keeps classification
running even if `Settings.auto_pick_sampling` is off, so sampling auto-tune
and model auto-pick can be toggled independently of each other.

**The Message improver rewrites the composer draft with the already-loaded
model — never a second model, never automatic.** "Improve my message", an
action next to Send in the composer (plus a standalone "Improve a message"
screen off the sidebar), sends the current draft through the same one-shot
completion path Writing Tools' Translate/Correct already use
(`crates/app/src/commands.rs::run_one_shot_completion`, generalized from
that feature) — local engine, cloud, or (on Android) the paired desktop's
`RemoteBackend`, whichever the user already has chat pointed at. A pure core
module, `crates/core/src/prompt_enhancer.rs`, owns the fixed rewrite prompt,
sampling, and an output cleaner that strips scaffolding (preambles, quotes,
code fences, trailing notes) and rejects anything that looks like an
answer, a refusal, a language switch, or a runaway/truncated rewrite —
those cases leave the draft untouched with a status message rather than
silently corrupting what the user typed. Gated by the same `writing-tools`
plugin capability as Translate/Correct (`writingToolsAvailable`) rather than
a Settings field of its own — off by default, since unlike the app's silent
auto-tuning helpers this adds a visible affordance and costs a generation
per press; enabling "Writing Tools" in Add-ons now reveals all three. The
rewrite always replaces the draft atomically (never streamed token-by-token
into the textarea) and is one Undo click or Escape away from being reverted
— native undo can't help here, since a programmatic textarea update doesn't
enter the browser's own undo stack. Counts toward the parental-controls
daily message cap only when parental controls are actually enabled, so the
vast majority of users (who never touch that feature) are never
double-charged for an edit that's always followed by a real, counted send,
while a capped child can't use it as an unlimited, friction-free channel to
the model. Original design (predating the move into the `writing-tools`
plugin) in `docs/design/PROMPT_ENHANCER.md`.

**Search and Projects: a plain scan, and one project per conversation.**
`store::search` (`crates/core/src/store.rs`) is a case-insensitive
character-by-character scan of every conversation's title and message
content — no embeddings, no index, consistent with "mono-model first" above
and the same call `agent::builtin::rerank` makes for web-page reranking. Fine
for a local desktop app's conversation count; title matches rank above
content matches, both by recency. Projects (`crates/core/src/projects.rs`,
mirroring `store.rs`'s shape) are just a name + timestamps; a project's
*membership* lives entirely on the conversation side
(`Conversation::project_id: Option<String>`), not as a list on `Project` —
moving a conversation is a single conversation-file write, never two records
kept in sync. This also means a conversation belongs to at most one project
at a time, by construction (simpler mental model, matches Claude.ai/ChatGPT
Projects) — there's no `project_ids: Vec<String>` to reconcile. Deleting a
project (`projects::delete`) unassigns every conversation that referenced it
rather than deleting them. Agent mode gets one deliberate, narrow exception
to its per-conversation workspace jailing: when the current conversation is
in a project, `list_project_conversations`/`read_project_conversation`
(`agent/builtin/project.rs`) let it consult sibling conversations in the same
project — reading raw content (BM25-ranked via `rerank::top_chunks` when
long, same as a fetched web page), gated by the same per-call consent every
other tool uses. Wired in per-turn by `agent_bridge::build_deps`, which looks
up the current conversation's `project_id` fresh on every call rather than
caching it.

**Remote Access: the one gated exception to "engine is loopback-only," never
the default path.** `crate::remote` (`crates/core/src/remote/`) lets another
device — today anything speaking HTTPS/JSON, eventually an OpenPal mobile
client — pair with this desktop and proxy chat completions to it over the LAN
or an already-connected overlay network (Tailscale/WireGuard/router
port-forward). Off by default, behind Settings → Devices. When on,
`RemoteAccessManager` binds an authenticated, TLS-terminated gateway
(`gateway.rs`, `axum` + `axum-server`) on `0.0.0.0:<persisted port>` —
deliberately not loopback, since the whole point is another device reaching
it — gated by a per-device bearer token minted at pairing time (QR code +
short manual-fallback code, single-use, ~5 minute session) and validated on
every request. Chat completions proxy as raw bytes straight to the engine's
own `/v1/chat/completions`, bypassing `EngineManager::chat_stream`/
`ChatEvent` entirely — a remote client speaks plain OpenAI-style SSE, not
this app's internal desktop-UI event shape. Also included: LAN discovery
(mDNS, `discovery.rs`) and wake-on-LAN send (`wol.rs`) plus a best-effort
"is WOL even enabled on this host" check (`wol_status.rs`) — WOL only wakes a
machine within its own LAN broadcast domain, a hard, documented limit, not
something this feature works around. OpenPal does not operate its own relay
for off-LAN reachability, keeping "local-first, no operated backend" intact;
see `docs/REMOTE_INFERENCE.md` for the full protocol, security model, and
the handoff to a future mobile client (a separate, complementary effort —
`docs/design/MOBILE.md` — covers *on-device* inference for mobile; this is the
phone borrowing a desktop's model instead).

## Gotchas

- **Tauri hook cwd**: `beforeDevCommand`/`beforeBuildCommand` run from the
  *parent* of the directory containing `tauri.conf.json` (here: `crates/`),
  not from the config dir itself — verified empirically with tauri-cli 2.11.
  That's why the hooks use `npm --prefix ../ui`, while `frontendDist` (which
  *is* config-dir-relative) uses `../../ui/dist`.
- **`time` crate pin**: `time` is held at 0.3.47 in Cargo.lock; 0.3.48 breaks
  `cookie 0.18` (E0119 coherence conflict). Re-check before `cargo update`.

## Data on disk

`~/Library/Application Support/com.myopenpal.openpal/`

- `models/*.gguf` (+ `.part` for resumable downloads; custom Hugging Face
  models download here as `custom-<slug>.gguf`)
- `app.sqlite` — the app-wide SQLCipher database (`openpal_core::app_db`),
  keyed by the data-encryption key minted/unlocked at boot
  (`crates/app/src/bootstrap.rs`). Holds what used to be separate plaintext
  files:
  - `conversations` table — one row per chat; a `data` column (the full
    conversation, pretty-printed JSON, byte-identical to the old per-file
    format) plus `title`/timestamps/`project_id`/etc. as a
    read-optimization cache re-derived on every save (see
    `openpal_core::store`)
  - `projects` table — one row per project (name + timestamps only; a
    conversation's own `project_id` field is what actually assigns it —
    see "Search and Projects" below)
  - `artifacts` table, keyed by `(conversation_id, id)` — files the model
    creates via the `create_artifact` agent-mode tool
  - `settings`, `custom_models`, `plugin_records`, `remote_access_config`
    tables — each a single-row `key='current'` JSON blob (per-plugin enable
    state + completed setup version, for `plugin_records`; enabled flag,
    persisted port, and self-signed TLS cert/key, for
    `remote_access_config` — see `docs/REMOTE_INFERENCE.md`)
  - `paired_devices` table — one row per device Remote Access has paired
    with (name + salted bearer-token hash + timestamps, never the token
    itself)
- `plugins/<id>/` — user-installed (community) plugins
- `plugin-data/<id>/` — a plugin's mutable state: `venv/`, downloaded sources
  (e.g. SearXNG), generated config, `setup.log`, `service.log`
- `logs/openpal.<date>.log` — the app's own `tracing` output, one file per day,
  newest 14 kept (older auto-deleted). See **Logging** below.
- `logs/engine.log` — llama-server output, truncated per start (debugging tool
  #1); the prior session is kept as `logs/engine.prev.log`
- `logs/plugin-chat.log` — chat-plugin sidecar stderr, truncated per turn (its
  stdout is the NDJSON event stream Rust consumes, never logged here)

## Engine lifecycle

```
start(model):  free port → spawn llama-server (-m model --port p
               --host 127.0.0.1 -c ctx -ngl 99 --jinja --no-webui + extraArgs)
               → poll /health (conn-refused → 503 loading → 200 ok, 180s cap)
               → Running { model_id, port }
chat:          POST /v1/chat/completions {stream: true} → parse SSE lines →
               ChatEvent::Token … → final chunk carries timings → Done{stats}
stop / exit:   child.kill(); kill_on_drop covers panics
```

## Logging

`crates/app/src/logging.rs` brings up `tracing` at startup (from
`AppState::initialize`, so the logs dir and the user's level setting are known).

- **Where**: a daily-rolling file `logs/openpal.<date>.log` via
  `tracing-appender` (non-blocking writer; the `WorkerGuard` lives in
  `AppState`). Stdout is added too in debug builds. The old stdout-only setup
  was discarded in packaged builds — there's no console.
- **Rotation/retention**: daily roll, newest 14 files kept.
- **Level**: `RUST_LOG` wins (dev); otherwise `Settings.logLevel`
  (Settings → Advanced → "Logs & diagnostics"), read at startup, so a change applies on the
  next launch. Default `info`.
- **Sources captured**: backend `tracing::*`, uncaught Rust panics (a panic hook
  logs location + backtrace), and the webview (`window.onerror` /
  `unhandledrejection` → `frontend_log` command, target `webview`).
- **In-app viewer**: Settings → Advanced → "Logs & diagnostics" tails the app or engine log
  (`get_recent_logs`), with "Open logs folder" / Copy / "Clear old logs". The
  engine API key and bearer tokens are masked (`logging::redact`) before logs
  are shown or copied.

## Adding a platform (Windows/Linux)

1. `bundler fetch-engine --target <platform>` mapping to the right llama.cpp
   release asset.
2. Platform branches in `hardware.rs` (already structured for it) and
   `open_models_folder`.
3. Tauri bundle targets in `tauri.conf.json` + signing flow in `bundler`.

Core logic, UI, catalog, and IPC need no changes.

## Adding a platform (Android/iOS)

Mobile is a different shape of change: the engine's managed-subprocess pattern
above and the Python plugin sidecars do not run under iOS/Android app
sandboxing, so it isn't a `bundler fetch-engine --target` addition — it needs an
in-process llama.cpp engine backend behind a new trait seam, and the Python
plugins dropped or replaced. Repo strategy, the engine-backend design, the
mobile plugin plan, and a phased migration path are in
[`docs/design/MOBILE.md`](docs/design/MOBILE.md).
