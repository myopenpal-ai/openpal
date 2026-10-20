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
  allow" grant store, one mutex guarding both. It also brokers `ask_user`
  answers and the prompt-injection review card's Continue/Stop.
- `audit.rs` — `AuditLog`: an append-only, hash-chained lifecycle trail in
  the app database's `agent_audit` table (see "Audit log" below for exactly
  what this does and doesn't guarantee).
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
   resolved paths, and any heuristic danger flags — plus
   `PromptInjectionSuspected` when something earlier in the same turn was
   flagged by the prompt-injection detector (see below). For that line the
   card waits on scans still in flight for at most 10 seconds. The flag
   never decides the call; the three buttons do.
4. The frontend's three-button card resolves it via `respond_tool_consent`:
   **Allow once**, **Always allow** (persists a grant — see below), or
   **Deny** (with an optional note that becomes the `tool`-role message
   content the model sees, so it can adjust its plan instead of blindly
   retrying).
5. Execution goes through the sandboxed/jailed path per tool; the result
   (or denial note, or rejection reason) is always what's fed back to the
   model — a call is never left unanswered. Before a result goes back it
   is checked for prompt injection (which results: see below), whichever
   way the call was approved: prompt, grant or no consent needed. A
   flagged result waits on a review card, and Stop ends the turn before
   the model reads it (see "Review before the model reads" below).

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

## Prompt-injection detection (`crates/core/src/injection/`)

Text the model reads can carry orders the user never gave: a web page telling
"AI assistants" to send `~/.ssh` somewhere, a README comment, a CV in white
text, a skill with one bad step. An always-on classifier looks at that text
on its way in and **flags** what reads like instructions aimed at the model.
When the flagged text is something the user didn't write (an attachment, a
skill, a tool result), the turn **pauses before the model reads it** and asks
the user to Continue or Stop. Nothing is ever rewritten, redacted or partly
withheld: the model reads the text exactly as it was, or not at all. A
detection never decides a tool call either. Every call still goes through the
same three-button prompt, and granted or consent-free calls still run without
one.

**Only turns that use tools are checked:** agent mode and the web-search turn.
That's where injected text can act without the user, by calling a tool or by
sending the conversation somewhere with `fetch_url`. Plain chat, cloud chat
and Writing Tools give the model no tools, so whatever injected text does
there (a slanted answer, a persuasive request, a link) still needs the user to
act on the reply. Checking them would cost every message a 3–15 s classifier
load, about 1 GB of RAM and false-positive pauses, so they aren't checked, by
design. Nothing they carried escapes the check: a tool turn re-checks the
conversation's whole model-bound history, so a file attached in plain chat is
reviewed the first time the conversation reaches a tool.

**A knowing exception to mono-model first.** This is a second, dedicated
model on every machine's default path, and there is no setting that turns it
off. See ARCHITECTURE.md → Design philosophy for why that trade was made
(issue #9).

**The model.**
- [Horizon-Labs/prompt-injection-guard-small](https://huggingface.co/Horizon-Labs/prompt-injection-guard-small):
  an mmBERT-small classifier (141M parameters, Apache-2.0, multilingual,
  French included) labelled `SAFE` / `INJECTION`.
- `injection/model.rs` pins one revision of two files: the publisher's own
  int8-embedding ONNX export (268 MB) and its `tokenizer.json` (34 MB), each
  checked by size and sha256. The tokenizer also carries the model's
  obfuscation normalizer: NFKC, and stripping invisible and tag characters.
- It runs in-process with `tract-onnx`, which is pure Rust with no runtime
  library to bundle or sign.
- Text is cut into 512-token windows overlapping by 64 tokens, and the text's
  score is its highest window score. It is flagged at P(injection) ≥ 0.5,
  the model's own decision boundary, deliberately not tuned on our samples.
- Conversation transcripts lose their `user:` / `assistant:` prefixes before
  scoring, because they look exactly like the forged turns injected text
  fakes.

**What is scanned, and where.**
1. **The start of every turn that uses tools** (`commands::chat_stream`).
   The turn's route (`TurnRoute`) is decided first, and only agent and
   web-search turns (`TurnRoute::scans`) go through the gate; plain and cloud
   turns are routed straight away.
   - What's scanned: each user message's typed text, the body of an invoked
     `/<skill>`, and attached documents. A document is scanned with its file
     name, exactly as the model receives it.
   - Documents and skill bodies are checked before the turn is routed, and
     the turn waits for them. Typed text is checked in the background once
     that check is through.
   - The whole history is re-sent every turn, so all of it is in scope,
     including messages sent earlier in plain chat. It is scanned newest
     first. Messages the user withheld from the model (see below) are neither
     sent nor scanned.
   - Text already scored this run is answered from a cache without waiting
     behind a scan in progress.
2. **Every tool result** (`orchestrator::run_and_report`), including those
   in a subagent's nested loop, after the tool ran and before the result
   goes back to the model:
   - `fetch_url` pages and `web_search` snippets;
   - `read_file`;
   - `run_shell` / `run_code` output;
   - `read_project_conversation` and `list_project_conversations`;
   - `ask_user` answers, which are the user's own words and are handled like
     typed text;
   - any tool this list doesn't know yet.

   Skipped: `write_file` and `create_artifact`, whose results are the app's
   own fixed confirmations, and `spawn_subagent`, whose result is the helper
   model's answer (its own tool results were scanned inside its loop).

Not scanned: plain and cloud chat turns, Writing Tools and the Message
improver; the model's own earlier replies; the system prompt (the user's own
setting); images; and tool error messages.

**What a detection does.** It depends on who chose the text.
- **A pause, for text the user didn't write.** An attached document, an
  invoked skill's body or a tool result waits on a review card before the
  model reads any of it: Continue sends it unchanged, Stop ends the reply
  without the model reading it. This holds in agent and web-search turns,
  for attachments, skills and tool results, granted and consent-free calls
  included. See "Review before the model reads" below.
- **A notice, for text the user typed or pasted.** The message being sent
  and an `ask_user` answer go to the model as written, since pressing Send
  was the decision. The reply gets a notice line instead: "Possible hidden
  instructions for the AI in your message. It was sent as you wrote it. If
  you copied it from somewhere, double-check this answer." Typed text in
  older messages only flags the consent card.
- **One app-log line** under the `prompt_injection` target, for example:
  `WARN prompt_injection: possible prompt injection in text sent to the model source="web_page" score=0.97 chars=18342`.
  - It records the route, the score and the length, never the text: the log
    is meant to be shared for support.
  - A given text is logged the first time it is flagged in a run. If it
    reappears in later turns it still flags those turns, but isn't logged
    again.
  - `logging::level_filter` always lets this target through, so even
    Settings → Advanced → Logs → "Errors only" doesn't hide a detection.
  - Each answer to a review card adds one line per item, again with the
    route and never the text or a name:
    `WARN prompt_injection: user decision on possible prompt injection source="attachment" decision="stopped" unchecked=false`.
- **A consent-card flag.** `AgentDeps.injection` (a `TurnGuard`, shared with
  subagents) remembers that the turn saw a detection. `dispatch_call` adds
  `DangerFlag::PromptInjectionSuspected` to the next consent card, after
  waiting up to 10 s for scans still in flight. The card shows one more
  warning line: "Something the AI read earlier looked like hidden
  instructions aimed at it. Double-check this is what you actually want."
  The line appears even when the user already chose Continue for that text.

**Review before the model reads** (`injection/review.rs`).
- **Two gates, one card.**
  - `TurnGuard::gate_user_side` runs once per agent or web-search turn,
    before routing. `commands::chat_stream` goes through
    `injection::run_guarded`, which only routes the turn after Continue, or
    when nothing needed asking. It covers the documents and skill bodies of
    every message that will be sent, and one card lists every flagged item,
    since they all go out in one request.
  - `TurnGuard::gate_tool_result` runs in `run_and_report`, after the tool
    ran and before its result goes back. Each flagged result gets its own
    card, in call order. A helper's result says which helper found it.
- **Events and the answer.**
  - `InjectionCheckStarted` / `InjectionCheckFinished` drive a "Checking…"
    status line, shown only when a check takes longer than 700 ms.
  - `InjectionReviewRequested` brings up the card. For each item it gives
    the route, a name (the file, the `/skill`, or the URL, query, path or
    command of the tool call) and an excerpt: the part of the text that
    tripped the check, shown as plain text, with invisible characters shown
    as `▯`. Only the first four items of a card carry an excerpt.
    - The excerpt is at most 1,500 characters. The classifier's most
      suspicious window is 510 tokens, about 2,000 characters of prose, so
      when a flagged window is longer than that, `review::locate` scores
      half-overlapping 1,500-character stretches of it and keeps the
      highest. That runs once per flagged text, right after it was scored,
      and the cached verdict keeps the result.
    - An attached file's name is cut at 160 characters on the card. A name
      cut that way stays in the excerpt whole, as the
      `[Attached file: …]` line the model reads, since an instruction can
      hide in a long name too.
  - The frontend answers with `respond_injection_review` (`continue` or
    `stop`). `ConsentBroker` brokers it the same way it brokers a consent
    card.
  - Each answer leaves one `InjectionNotice` per item, saved on the reply as
    `injectionNotices`. A notice holds the route, the name, the outcome and,
    for user-side text, the message's id. It never holds the excerpt.
    Excerpts are never stored or logged.
- **Continue.** The text reaches the model byte-for-byte unchanged. The
  choice is remembered in memory for this conversation and this run of the
  app, keyed by conversation, route and the text's hash, so the re-sent
  history doesn't ask again. It is never saved, because an approval stored
  with the conversation could be forged by importing or syncing one. After a
  restart the user is asked once more.
- **Stop on an attachment or skill.** No request is sent and the reply keeps
  only its "stopped" notice. The user messages that carried the text are
  saved with `withheldFromModel: true` and tagged "Not sent to the AI" in
  the chat. From then on they, and the replies after them, are left out of
  every request (`engine::model_bound`) and are never asked about again.
  The flag can only take text out of a request, so a forged one is
  harmless. Retry clears it and asks again, and Edit sends a new message,
  which is checked again.
- **An item from an earlier message.** The usual way a file attached in plain
  chat meets the card is a later tool turn, after the model already read it
  in earlier replies. When an item's `messageId` isn't the turn's own user
  message, the card says so instead of promising the AI won't read it: "The
  AI read this earlier. Check it before the AI uses tools." and "Stop keeps
  it out of everything the AI reads from now on, and this reply ends." The
  "stopped" notice words it the same way. Stop works as above: the earlier
  message and the replies after it are withheld from then on.
- **Stop on a tool result.** The turn's cancel flag is set. The rest of that
  round's calls are denied as "stopped", the loop ends with `Done`, and no
  further request is made. Because a helper shares the turn's cancel flag,
  this also ends the turn when the result came from a helper's loop.
- **Audit.** A tool result's review adds an `injectionReview` entry for the
  call to the audit trail (the hash-chained `agent_audit` table in the
  encrypted app database, see "Audit log" below), with the decision
  `continued` or `stopped`, after the call's usual `finished` entry. The
  entry records an answer to a card, so it is written only when a card was
  shown. A Stop pressed while the tool ran or while its result was being
  checked ends the turn the same way, but adds no entry
  (`review::Review::Interrupted`).
- **Dismissing never lets text through.** Escape means Stop. The card
  focuses itself, never a button, so a stray Space or Enter answers
  nothing; and it doesn't take the focus at all while another dialog or
  preview is open or the user is typing a message, so a key meant for
  those can't answer it either. The Stop button counts as Stop while a
  check is running or a card is up; a notice is written only if a card was
  showing. The turn claims the generation slot before the gate, which is
  why the Stop button can reach it. A reload of the window (F5 stays live
  in WebView2) stops the running turn the same way, since nothing on the
  new page could answer its card. The gate also runs inside its own
  `begin_generation` / `end_generation` bracket (every checked turn is
  local), so auto-unload can't fire under an open card.

**Lifecycle and cost.**
- **Download.** The two files download silently in the background after
  unlock (`Detector::download_until_present`) into `<data>/prompt-guard/`, on
  every desktop install, so they're there the moment someone turns agent
  mode or web search on. Not on Android, where no turn uses tools. The
  download is resumable, refuses to start without 1 GB of free disk to spare,
  and retries with backoff (30 s, 2 min, 10 min, then hourly).
- **Load and unload.** The classifier is in memory only around tool turns.
  Nothing loads it ahead of time: the first scan of an agent or web-search
  turn does, and that turn holds it (`TurnGuard::for_tool_turn`, a
  `ClassifierHold`) until it ends, so a card left open or a long tool run
  never sees it unloaded halfway. Once no turn holds it, the app's idle loop
  drops it after 5 minutes without a scan, counted from the later of its
  last scan and the end of the last turn that held it, whatever the chat
  model is doing. Unload, auto-unload and deleting the running model drop it
  too. On an Apple M4 a load takes 3.2–3.7 s, or about 14.5 s once from a
  cold disk, so the first tool turn after 5 idle minutes shows "Checking…"
  for that long, well inside the gate's 60 s wait. With its tokenizer it
  uses about 0.8–1 GB of RAM on top of the chat model while it's loaded.
  Model recommendations (`selection.rs`) don't reserve room for it.
- **Speed.** Scans run one at a time, on at most half the CPU cores (4 at
  most). On an M4 that is about 0.3 s per 512-token window: 3.4 s for a
  20,000-character page, about 34 s for a 200 KB tool result (the
  `MAX_TOOL_OUTPUT_BYTES` cap), and well under a second for a short message.
- **Who waits.** Attachments, skills and tool results are checked in the
  foreground: the turn waits for their verdict.
  - A check that takes longer than 700 ms (`CHECKING_STATUS_AFTER`) shows
    "Checking…" in the reply's status line.
  - A gate waits at most 60 s (`GATE_SCAN_WAIT`) for a verdict. On the user
    side that covers all of the turn's attachments and skills together.
    After that the user is asked, with the item marked as not checked. The
    scan carries on, so its verdict is still cached and logged.
  - Typed text is checked while the reply is written. Once the reply is
    done, the turn gives its notice up to 2 s more to arrive
    (`NOTICE_GRACE`); a verdict that comes later only flags and logs.
- **Pre-scoring at pick time.** In a chat with agent tools or web search on,
  `pick_attachments` (with `prescan`) starts scoring each picked document in
  the background, in the same form the gate checks it. The verdict is then
  usually cached before Send is pressed, and the pick loads the classifier
  while the user is still typing. One side effect: a detection's log line is
  written when the file is picked, not when it's sent. Plain chat and
  Writing Tools don't pre-score.
- **When unavailable.** While the files are missing, scans are skipped and
  one warning per run says detection is unavailable. If they fail to load,
  each attempt logs its reason and loading is retried every 10 minutes.
  Either way, text goes through unchecked, with no pause and no notice. A
  classifier that is there but fails on one particular text, or has no
  verdict within 60 s, is different: the user is asked, with a "Couldn't
  finish checking" item, so a text crafted to make the check fail can't
  slip through.

**How well it works.** It was measured on the documented sample sets in
`crates/core/tests/fixtures/prompt_injection/`: 163 English and French
samples across all eight routes. That folder's README has the full
breakdown, and `tests/prompt_injection_corpus.rs` re-checks it against the
real model.
- 73 of 75 benign samples pass. The 2 false positives are raw chat-completion
  JSON in command output, and one email-drafting transcript.
- 69 of 76 injection samples are flagged, including the text from issue
  #9's own report, attached as a file.
- **Known blind spot: skills.** 7 of 9 skills with a hidden malicious step
  are missed. A skill is the user's own instructions to the model by design,
  so a bad instruction inside one reads like a legitimate request. Blatant
  payloads in skills are still caught.
- A page that merely *quotes* an injection payload, for example a security
  write-up, may be flagged. That is accepted, but it isn't free: a false
  positive on anything the user didn't write (a page, a file, an attachment,
  a skill) costs a pause until the user answers the card, and one on a typed
  message costs a notice line. Continue keeps it from asking again in the
  same conversation.

**Not in this design.**
- Checking plain chat, cloud chat, Writing Tools or the Message improver.
  Their model has no tools; a file they carried is checked once the
  conversation reaches a tool turn.
- Pausing on typed or pasted text, or on `ask_user` answers: those get a
  notice only.
- Redacting flagged text, withholding one attachment out of a message, or a
  third "skip this and keep going" answer.
- Saving Continue choices or the score cache to disk.
- Holding text back while the classifier is missing or still downloading.
- Showing scores, or a setting that turns detection off.
- Scanning tool error messages.
- Checking what a paired device sends through the Remote Access gateway
  (`remote/gateway.rs`), which goes straight to the engine.
- Wiring up `heuristics::derived_from_recent_output`, which is still unused.

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

## What a reply can still do without tools

With agent mode off — or on, between tool calls — the model's only output is
text, rendered by `ui/src/components/Markdown.tsx`. Text steered by hidden
instructions (a web page, a file, a model card) cannot reach the network on
its own from there:

- **Images never load.** A placeholder with the alt text stands in, so an
  image address packed with conversation text is never fetched.
- **Links ask first.** No link holds a live `href`, so hovering, pressing or
  rendering one causes no DNS lookup or connection. Clicking an http(s) link
  opens a dialog showing the real host (an IDN in its `xn--` form, the host
  behind any `user@`), the full address, a warning when the link text names a
  different host or the destination is local, and Open link / Cancel / Copy
  address. Focus starts on the dialog and Escape cancels. Nothing is
  remembered per site.
- **Every other link is inert** — `mailto:`, relative, `//host`, and the
  emptied `javascript:`. Only in-page `#` links keep working.

What remains is persuasion: a reply can still ask the user to open a link
whose address carries what it was told to leak, or to copy text (including a
code block) somewhere. The dialog's full address and advice line are the
defence there; nothing can stop a user who reads them and chooses Open.
OpenPal's own update notes are the one place links open without asking.

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

The audit log is a lightweight, hash-chained lifecycle trail kept as rows
of the `agent_audit` table in the app-wide, SQLCipher-encrypted database —
not a standalone file (`agent/audit.rs`). Each row's `prevHash` is the
SHA-256 of the previous row's exact `line` bytes. Honest about its limits:
it **detects** a retroactive edit to an already-written row and pinpoints
where history diverges; it does **not** stop someone holding the database
key from rewriting the whole chain from a fabricated genesis, nor a
rollback to a stale-but-valid backup (real non-repudiation would need
external signing/timestamping — out of scope). It also doesn't carry full
argument/output content — that's the (deletable) conversation transcript's
job; the audit log only ever sees a short, already-capped preview.
