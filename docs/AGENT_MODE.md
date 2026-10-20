# Agent mode

Agent mode lets the local model call tools that act on the user's real
machine — run a shell command, run a short script, read a file, write a
file — instead of only talking. It is off by default (`Settings.agentModeEnabled`)
and requires that switch to be on for a turn to route through it
(`commands::chat_stream`). The workspace folder tools are jailed to is not a
user choice: it's always `<data_dir>/agent-workspaces/<conversation_id>`,
created on demand by `commands::agent_chat_turn`, so each conversation's agent
calls get their own folder that no other conversation's tool calls can touch.
The one deliberate exception is `list_project_conversations`/
`read_project_conversation` (see "Project consultation" below), which cross
into sibling conversations *within the same user-created project* — every
other tool stays jailed exactly as described here.

## Why this is Rust-native, not a plugin

The existing "agentic" precedent — the `web-search` plugin — runs its
tool-calling loop in a Python sidecar (see `plugins/PLUGINS.md`). Agent mode
doesn't: every tool it exposes needs OS-level sandboxing or filesystem
jailing, which — per this app's own security boundary ("the frontend only
talks to our own commands; everything else happens in Rust") — can only live
in Rust anyway. Splitting the reasoning loop into Python would buy narrative
consistency with `web-search` and nothing else, while costing the ability to
ship in the portable build (which already cuts every Python-venv plugin) and
a generic tool abstraction. So the whole thing — the tool-calling loop, every
built-in tool, sandboxing, consent, and the audit log — is a Tauri-free
module in `crates/core::agent`, talking to `llama-server` directly over the
same OpenAI-compatible endpoint `EngineManager::chat_stream` uses (see
`agent::orchestrator::run_agent_turn`). `engine.rs` and `sse.rs` are
untouched — the orchestrator parses its own SSE stream so it can see
`delta.tool_calls`, which `crate::sse::drain_sse` doesn't know about.

## Module map (`crates/core/src/agent/`)

- `tool.rs` — the `Tool` trait every built-in tool implements (`id`,
  `origin`, `spec`, `consent_summary`, `grant_key`, `execute`), plus the
  wire-facing `ConsentSummary`/`ResolvedPath`/`DangerFlag` shapes.
- `registry.rs` — `ToolRegistry`, the OpenAI `tools` array builder.
- `workspace.rs` — `WorkspaceJail`: path resolution + a category-broadened
  denylist (credentials, OS persistence surfaces, the app's own data dir)
  applied to **every** resolved path, whether inside or outside the
  configured workspace, plus a TOCTOU-safe (`O_NOFOLLOW` + post-open fd
  identity recheck) file open for `read_file`/`write_file`. There's no OS
  sandbox here — Rust's own process makes the `open`/`read`/`write` calls
  directly, so this module *is* the containment.
- `sandbox/` — the `Sandbox` trait, `SeatbeltSandbox` (macOS
  `sandbox-exec`), and `NoSandbox` (every other platform — returns
  `Unsupported` rather than ever running a command unconfined; the Settings
  toggle must be hidden, not merely disabled, wherever this is the platform
  sandbox).
- `builtin/` — the built-in tools: `run_shell`, `run_code`, `read_file`,
  `write_file`, `spawn_subagent`, `web_search`/`fetch_url` (plain-chat web
  search reuses this same registry, scoped down — see ARCHITECTURE.md),
  `ask_user` (see "Asking a clarifying question" below), and
  `list_project_conversations`/`read_project_conversation` (see "Project
  consultation" below).
- `consent.rs` — `ConsentBroker`: pending per-call approval (a
  `oneshot::Receiver` per in-flight request) plus the persisted "Always
  allow" grant store, one mutex guarding both.
- `audit.rs` — `AuditLog`: an append-only, hash-chained NDJSON lifecycle
  trail (see "Audit log" below for exactly what this does and doesn't
  guarantee).
- `fetch_allowlist.rs` — `FetchAllowlist`: which URLs `fetch_url` may open
  this turn, and its 10-fetch budget (see "Where the web tools may send
  requests" below).
- `heuristics.rs` — non-blocking danger-flag detectors (fork-bomb shape,
  raw device I/O, curl-pipe-shell, sudo, "does this look copied from a
  recent tool's own output") shown as warning badges on the consent card.
  These never gate execution on their own — every call still goes through
  the same three-outcome prompt.
- `llm_tools.rs` — `ToolCallAssembler`: reassembles a streamed
  `delta.tool_calls` round (fragmented by index, arriving out of order)
  into complete calls, with per-call and combined byte caps so a
  malicious/broken stream can't grow memory unboundedly.
- `orchestrator.rs` — `run_agent_turn`, the loop tying all of the above
  together.

## The Seatbelt sandbox (`run_shell`/`run_code`)

Two macOS-specific things worth knowing before touching `sandbox/macos.rs`
or the `.sb` profiles, both found empirically (see the profiles' own doc
comments for the full detail):

1. **Read is broad, not a fixed allowlist.** dyld's shared cache lives
   under a Cryptexes mount whose exact path is a macOS-version-specific
   implementation detail (`/System/Volumes/Preboot/Cryptexes/OS/...` on the
   version this was built against); denying reads outside a curated list of
   paths made dyld abort the process before it could even exec (confirmed
   via crash-report inspection — `dyld4::CacheFinder`/`ignition_halt`). So
   `file-read*` is allowed broadly and then narrowed back down: every user's
   home directory and mounted volumes are explicitly denied, then
   re-allowed just for the configured `WORKSPACE`/`SCRATCH` params via
   Seatbelt's most-specific-rule-wins resolution — verified empirically that
   this still confines reads correctly even when the workspace sits inside
   the denied home directory. `file-write*` stays a narrow positive
   allowlist (`WORKSPACE`+`SCRATCH` only) throughout — that's the control
   that actually matters for tamper/persistence, and it was never widened.
2. **`-D`-bound paths must be pre-canonicalized.** Seatbelt's `subpath`
   match is against the kernel's resolved view of an access. `$TMPDIR` on
   macOS is expressed through the `/var` → `/private/var` symlink, so a
   `WORKSPACE`/`SCRATCH` param bound to the unresolved form silently never
   matches real accesses. `SeatbeltSandbox::run` canonicalizes
   `workspace`/`cwd`/`scratch` itself before binding them — don't trust a
   caller to have already done this.
3. **`RLIMIT_AS` isn't enforceable on Darwin** (`setrlimit`/even plain
   `ulimit -v` return `EINVAL`) — memory exhaustion is bounded instead by
   `RLIMIT_FSIZE`, the disk-quota watchdog, and the wall-clock timeout, not
   a hard address-space ceiling.

`run_code` is **not** a smaller-blast-radius sandbox than `run_shell` —
`shell.sb`'s exec allowlist already covers `python3`/`node`/`bash`, so
anything `run_code` can do, an approved `run_shell` call could already do.
They share the same effective capability ceiling; `run_code`'s value is
structure (no shell-quoting hazards), not isolation.

Command/code text is written to a private (`0600`) scratch file and the
interpreter is invoked against that file's path — never via `-c "<text>"` —
so a secret a user inlines into a command isn't visible to `ps` from other
processes on the same account for the run's lifetime (verified in
`command_text_never_appears_in_argv`).

## Consent protocol

Every tool call — regardless of tool, regardless of an existing grant —
goes through `dispatch_call` in `orchestrator.rs`:

1. `consent_summary()` resolves/validates the call. A malformed or
   jailed/denylisted call is rejected **here**, before any prompt —
   `ChatEvent::ToolCallRejected`, never shown to the user.
2. `grant_key()` is checked against the persisted store. A hit still emits
   `ChatEvent::ToolCallStarted{auto_approved: true}` — never silent, just
   no prompt.
3. A miss registers a pending oneshot and emits
   `ChatEvent::ToolConsentRequested` with the summary, detail,
   resolved paths, and any heuristic danger flags.
4. The frontend's three-button card resolves it via `respond_tool_consent`:
   **Allow once**, **Always allow** (persists a grant — see below), or
   **Deny** (with an optional note that becomes the `tool`-role message
   content the model sees, so it can adjust its plan instead of blindly
   retrying).
5. Execution goes through the sandboxed/jailed path per tool; the result
   (or denial note, or rejection reason) is always what's fed back to the
   model — a call is never left unanswered.

**Grant keys are deliberately narrow.** `write_file`'s grant key includes a
hash of the content being written, not just the path+mode: a path-only key
would let a later, differently-worded write to the same path silently
re-execute under a standing "always allow rewriting this file" grant —
including a prompt-injected malicious overwrite riding in on content the
model read from somewhere else. Content-hashing makes "Always allow"
behave like "Allow once" whenever the content actually changes.
`read_file`/`run_shell`/`run_code` grants outside the configured workspace
expire after 30 days; workspace-relative grants don't (their key is already
narrow).

## Where the web tools may send requests

`web_search` and `fetch_url` run with no consent card, in the plain-chat
web-search turn and inside the `web-research` helper alike. They are the only
tools that can reach the internet without one, so the rules that bound where
a request may go live in code, not in a prompt:

- **`web_search` reaches only the local SearXNG.** Its client follows no
  redirect, so SearXNG's answer to an external bang (`!!g …`, which
  redirects to the bang's site with the query in the URL) comes back as an
  error. Every `!`-prefixed word is stripped from the model's query
  before it is sent anyway.
- **`fetch_url` opens only a URL from a trusted source.** The URL must
  exactly match (fragment aside, compared in parsed form) one taken from:
  - the user's own messages: typed text, invoked skill bodies, document
    attachments;
  - the user's system prompt;
  - a free-typed `ask_user` answer (not one of the model's own options);
  - a `web_search` result;
  - an earlier answer's citations.

  Anything the model or a tool wrote never counts: its own replies, a
  fetched page's links, command output, a helper's `task`. Adding anything
  to an allowed URL (a query parameter, a path segment, a subdomain, a
  `user@`) breaks the match. `FetchAllowlist` lives on `AgentDeps`:
  `run_agent_turn` seeds it from the history, and `WebSearchTool` and
  `AskUserTool` add to it as the turn goes. A helper's clone of `AgentDeps`
  shares it with the turn that spawned it.
- **At most 10 fetches per top-level turn**, helpers included.
- **Refusals happen in `consent_summary`, before any network or DNS
  traffic.** `url_is_blocked` resolves the host, and a
  `<secret>.attacker.example` name would leak through DNS alone. A refusal
  is an ordinary `ToolCallRejected`: the steps panel shows "Blocked
  automatically", and the reason tells the model to search first. There is
  no consent card to fall back on. Only the host is logged, never the full
  URL.

What this doesn't close:

- **Search queries reach third-party engines.** SearXNG passes the
  model's query to the public engines it aggregates, and that query can
  carry conversation text. Turning on web search is the consent to that.
- **The choice of URL is a low-bandwidth channel.** Injected text can
  encode data in which search result (or which URL in an attached file)
  the model opens. The budget of 10 fetches bounds it. Page links stay
  unfetchable for this reason: making them fetchable would widen the
  channel.

## Asking a clarifying question (`agent/builtin/ask_user.rs`)

`ask_user` is the one built-in tool whose "execution" is entirely an
interactive round-trip with the person, not an action on their machine — it
lets the model ask a short question (optionally with 2-4 suggested
quick-reply options; the person can always type something else) instead of
guessing at an ambiguous or underspecified request. Also registered on the
plain-chat web-search turn (`agent_bridge::build_web_search_deps`), not just
full agent mode — a vague search request benefits from it just as much, and
it costs that registry nothing to include.

It skips the consent protocol above entirely (`requires_consent` returns
`false`, same as `web_search`): asking a question has no blast radius. What
it needs instead is a way to block on an answer that hasn't happened yet, so
it reuses `ConsentBroker`'s pending-oneshot-per-call-id machinery — a second,
unrelated pending map on the same struct (see that file's own doc comment for
why it lives there rather than in a second broker type) — via a new
`ToolContext::consent` handle threaded into every tool, not just the
orchestrator's own consent gate. `execute()` registers a pending answer,
emits `ChatEvent::ClarifyingQuestionAsked`, then races the frontend's
eventual `respond_clarifying_question` call against the turn's own
cancellation signal (`orchestrator::wait_for_cancel`) — the same race
`dispatch_call` itself runs while awaiting a consent decision, just with a
free-text answer as the payload instead of Allow/Deny.

**A tool's own function-calling `description` isn't enough of a nudge on its
own.** Local models — especially smaller/general-purpose ones without
tool-calling-specific fine-tuning — often don't reliably reach for a tool
based only on its OpenAI-schema description; they'll answer (or ask) in
plain text instead of emitting a real tool call, even when the schema is
right there. `agent_bridge::ASK_USER_FRAMING` is appended to the system
prompt in both `agent_chat_turn` and `web_search_chat_turn` for exactly this
reason — mirroring `WEB_SEARCH_FRAMING`'s role for `web_search`/`fetch_url` —
so the model is told, at the framing level, that asking is an expected move,
not just one more function it might overlook. It's deliberately narrow (only
nudges `ask_user`, not "use your tools eagerly" generally): the consent-gated
tools are gated *because* their use should stay deliberate, and a broader
"reach for your tools" framing would cut against that.

This doesn't fully solve unreliable tool-calling on weak local models — see
`ASK_USER_FRAMING`'s own doc comment. There is currently no per-model
tracking anywhere in this codebase of whether a loaded model's chat template
actually supports emitting tool calls at all (`Settings.agent_mode_enabled`
and the composer's toggles are the only gates, regardless of which model is
loaded), and the old Python web-search sidecar's `tool_mode: auto/native/prompt`
fallback for incapable models (a static small-model denylist, plus a
retry-as-plain-text-JSON-action mode on a detected native-mode failure —
see its retirement commit, `7b6079e`) was never ported to this Rust
orchestrator for any tool, `ask_user` included.

## Project consultation (`agent/builtin/project.rs`)

The one deliberate crack in the per-conversation workspace jail described
above: when the current conversation belongs to a project (see
ARCHITECTURE.md's "Search and Projects"), the model can look at *sibling*
conversations the user explicitly grouped together, via two tools —
`list_project_conversations` (titles/message counts only) and
`read_project_conversation` (a specific sibling's content, optionally
BM25-ranked against a `query` the same way `fetch_url` ranks a long page —
`rerank::top_chunks`, reused verbatim, no second model). Both go through the
same consent prompt as every other tool; nothing here is silent.

Neither tool is registered at all unless the current conversation actually
has a `project_id` — `agent_bridge::build_deps` looks this up fresh on every
turn (never cached, since a conversation can be moved between projects
mid-session) and only adds them to that turn's `ToolRegistry` when there's a
project to scope them to. Inside `execute()`, `resolve_sibling()` re-checks
that the requested conversation's own `project_id` still matches before
touching its content — so even a model that somehow got a stale id (or one
belonging to a since-changed project) can't read across a project boundary.
This is `run_shell`/`read_file`'s "reject before any prompt is shown"
posture, just checking project membership instead of a path.

## Out of scope, and why this doesn't block them later

- **GUI computer-control, MCP client / third-party integrations.**
  `Tool::origin()` defaults to `ToolOrigin::BuiltIn`; a future MCP client
  adapter overrides it to `ToolOrigin::Mcp`. When that lands, an MCP-sourced
  tool's consent card **must** show the originating server identity and be
  marked unsandboxed/externally-executed by default — it has no Seatbelt
  equivalent, so its safety rests entirely on trusting that server, unlike
  the built-ins here.
- **Non-macOS sandbox backends.** `Sandbox` trait + `NoSandbox` stub are
  already in place; the Settings toggle must stay hidden (not merely
  disabled) on a platform where `platform_supports_sandbox()` is false.
- **Auto-approval tiers / session-wide policy.** Every tool call gets its
  own prompt, full stop — there is no "trust this tool for the rest of the
  session" mechanism, by design.

## Audit log

`logs/agent-audit.log` is a lightweight, hash-chained lifecycle trail (each
line's `prevHash` is the SHA-256 of the previous line's exact bytes) —
honest about its limits: it **detects** a retroactive edit to an
already-written line; it does **not** prevent an attacker with filesystem
write access from truncating the file and starting a fresh, internally
consistent chain from a fabricated genesis (real non-repudiation would need
external signing/timestamping — out of scope). It also doesn't carry full
argument/output content — that's the (deletable) conversation transcript's
job; the audit log only ever sees a short, already-capped preview.
