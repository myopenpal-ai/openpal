# OpenPal plugins

A **plugin** extends OpenPal at runtime. It can be enabled/disabled, may need a
one-time **setup** (e.g. install a Python environment), may run a long-lived
**service** (a managed sidecar), and may handle a **chat** turn. The first
plugin is `web-search`; the framework is built so more (including community
plugins) can drop in later.

This file is the contract shared by the Rust framework
(`crates/core/src/plugin.rs`, `crates/app/`), the TypeScript UI
(`ui/src/`), and each plugin's own code.

## 1. Where plugins live

A plugin is a **directory** containing a `plugin.json` manifest. The framework
discovers plugins from (first match by `id` wins):

1. Built-in: `<repo>/plugins/<id>/` in dev, `<app>/Contents/Resources/plugins/<id>/` packaged.
   A dev build scans the repo before its resource dir, so a copy staged there
   by an earlier `build` can't shadow your edits.
2. User-installed (community, future): `<app-data>/plugins/<id>/`.

**Experimental plugins sit behind "Experimental features"** (Settings →
Advanced, `Settings.experimental_features`, off by default). A plugin opts in
with `"experimental": true` in its manifest (§2); every add-on shipped today
does. While the switch is off, an experimental plugin behaves as disabled
whatever its own Add-ons switch says: `list_plugins` omits it (so every
surface gated on it disappears, and the Plugins sidebar entry too once
nothing is left to list), `capability_enabled`/`active_service_plugin`
report it off, `set_enabled(.., true)` is refused, and turning the switch
off stops its running service. A core (§9) plugin marked experimental is on
exactly while the switch is. The per-plugin enable records are never
rewritten, so turning it back on restores each plugin as it was, and a
plugin graduates by dropping the flag.

A renamed plugin keeps its users' enable record: add the `(old, new)` id pair
to `RENAMED_PLUGIN_IDS` in `crates/core/src/plugin.rs` and the record is
carried over on load (and a stale copy under the old id is not discovered).

A plugin's directory is treated **read-only**. All mutable state (the venv,
downloaded sources, generated config, logs) lives in the plugin's **data dir**:

```
<app-data>/plugin-data/<id>/        # OPENPAL_PLUGIN_DATA
    venv/                           # python venv the framework creates
    service.log                     # service stderr (truncated per start)
    setup.log                       # setup stderr (truncated per run)
    …                               # whatever the plugin writes (e.g. searxng-src/, settings.yml)
```

Enable state + setup version are persisted in `<app-data>/plugins.json`:
`{ "<id>": { "enabled": true, "setupVersion": "0.1.0" } }`.

## 2. Manifest (`plugin.json`)

```jsonc
{
  "id": "web-search",                  // stable, dir name; [a-z0-9-]
  "name": "Web Search",
  "description": "Let your assistant search the web and cite its sources.",
  "version": "0.1.0",
  "author": "OpenPal",
  "capabilities": ["chat"],            // what it plugs into; v1 understands "chat"
  "core": false,                       // optional, default false — see §9
  "experimental": true,                // optional, default false — see §1
  "python": {                          // omit for non-python plugins
    "requirements": "requirements.txt",   // pip-installed into the venv during setup
    "setup": "setup_plugin.py",            // optional first-run setup script (see §3)
    "service": "service.py",               // optional long-lived service launcher (see §4)
    "serviceHealthPath": "/healthz",       // GET path that returns 2xx when the service is up (default "/")
    "chat": "sidecar.py"                   // optional chat-turn handler (see §5)
  }
}
```

The Rust `PluginManifest` mirrors this (serde camelCase). Unknown fields are
ignored; all of `python.*` are optional.

## 3. Setup — one-time install (Rust → `setup_plugin.py`)

Run when a plugin is enabled and not yet set up for its current `version`
(idempotent; also the "Retry" path). The framework, in order:

1. Creates the venv if missing: `<basePython> -m venv <data>/venv`.
2. If `requirements` is set: `<venv>/bin/python -m pip install -r <dir>/<requirements>` (output → `setup.log`).
3. If `setup` is set: runs `<venv>/bin/python <dir>/<setup> --data-dir <data>`
   with cwd = plugin dir and env `OPENPAL_PLUGIN_DATA=<data>`, `PYTHONUNBUFFERED=1`.

`setup_plugin.py` writes **newline-delimited JSON progress** to **stdout**
(stderr is a free log → `setup.log`), flushing each line:

| line                                                        | meaning |
|-------------------------------------------------------------|---------|
| `{"type":"progress","message":"Downloading SearXNG…","fraction":0.4}` | `fraction` (0..1) optional |
| `{"type":"log","message":"…"}`                              | informational; not shown prominently |
| `{"type":"done"}`                                           | setup succeeded (terminal) |
| `{"type":"error","message":"user-safe sentence"}`          | setup failed (terminal) |

On success the framework records `setupVersion = manifest.version`.

## 4. Service — long-lived sidecar (Rust-managed)

If `service` is set, the framework runs it like the engine: pick a free
`127.0.0.1` port, spawn `<venv>/bin/python <dir>/<service> --port <port>
--data-dir <data>` (cwd = plugin dir, env as above, `kill_on_drop`, stderr →
`service.log`), then poll `http://127.0.0.1:<port><serviceHealthPath>` until a
non-5xx response (≤120 s) → `Running { port }`. The service is started when the
plugin is enabled (after setup) and on app launch for already-enabled plugins;
stopped on disable and on app exit. The service **must** bind only loopback.

## 5. Chat — handle a turn (Rust → `chat` script, per turn)

When an enabled, ready, `chat`-capable plugin exists, a chat turn is routed to
its `chat` script instead of the plain engine. The framework runs
`<venv>/bin/python <dir>/<chat> --data-dir <data>` (cwd = plugin dir, env as
above), writes **one JSON request** to stdin, and reads **NDJSON events** from
stdout (stderr → a per-turn log). Request:

```jsonc
{
  "base_url": "http://127.0.0.1:<engine-port>/v1",   // llama-server OpenAI base
  "api_key": "…",                                     // engine bearer token
  "model": "qwen3-4b-instruct-2507",
  "messages": [ {"role":"user","content":"…"}, … ],
  "system_prompt": "…",                               // base personality
  "temperature": 0.7, "top_p": 0.95, "top_k": 40, "min_p": 0.05,
  "repeat_penalty": 1.1, "repeat_last_n": 64,
  "presence_penalty": 0.0, "frequency_penalty": 0.0,
  "max_tokens": -1, "seed": -1,               // -1 = unlimited / random
  "data_dir": "<plugin data dir>",
  "service_url": "http://127.0.0.1:<service-port>",    // this plugin's own service, or "" if none
  // web-search knobs (a chat plugin reads what it needs, ignores the rest):
  "max_results": 6, "max_fetch": 4, "rerank": true,
  "rerank_model": "BAAI/bge-reranker-v2-m3", "rerank_top_k": 4,
  "max_steps": 4, "tool_mode": "auto"
}
```

The event stream is the existing web-search NDJSON contract (unchanged):
`status` / `search` / `result` / `fetch` / `citation` / `token` / `done` /
`error`, mapped by Rust to `ChatEvent` (`SearchStarted`/`SearchResult`/
`Fetching`/`Citation`/`Token`/`Done`/`Error`). The web-search plugin maps
`service_url` → its SearXNG base.

## 6. Rust ↔ TS types & commands

`PluginInfo` (flat; returned by `list_plugins`):
`{ id, name, description, version, author, capabilities: string[], enabled,
   setup: SetupState, service: ServiceState, hasChat, experimental }`.

`SetupState` (adjacently tagged, camelCase):
`notSetup | settingUp{message, fraction?} | ready | failed{message}`.

`ServiceState`: `stopped | starting | running{port} | failed{message} | notApplicable`.

`PluginEvent` (streamed by `set_plugin_enabled` while enabling):
`setup{message, fraction?} | ready | failed{message}`.

Commands:
- `list_plugins() -> PluginInfo[]`
- `set_plugin_enabled(id, enabled, onEvent: Channel<PluginEvent>) -> ()`
  — enabling runs setup (streaming) then starts the service; re-calling with
  `true` on a failed/incomplete plugin retries. Disabling stops the service.
- `chat_stream(messages, onEvent)` — unchanged name/signature; now routes to an
  enabled chat plugin when one is ready, else the plain engine.

`Settings` no longer has `webSearchEnabled` / `searxngUrl` (replaced by the
managed service). `web-search` is an experimental add-on (§1): its Plugins
switch is the global gate, and the composer's globe then toggles web search
for the current conversation only.

## 7. Service-capability plugins (e.g. `tts` / `stt`)

A `chat` plugin is driven *per turn* as a subprocess (§5). A plugin can instead
expose a capability that the app calls **directly over its managed loopback
service** (§4) — used by the `speech` plugin, which advertises `tts`
(text-to-voice) and `stt` (voice-to-text) and serves them from one `service.py`
(its endpoints are in `plugins/speech/PROTOCOL.md`).

The framework treats these like any other service plugin — it sets the plugin up
and runs/health-checks its service on enable. To consume a capability, the app:

1. finds the enabled, set-up plugin that advertises it —
   `PluginManager::active_service_plugin("tts" | "stt")` (the service analogue of
   `active_chat_plugin`; it requires the plugin to declare the capability *and* a
   `service`),
2. gets the running port with `ensure_service(id)`,
3. calls the plugin's HTTP endpoints on `127.0.0.1:<port>`.

The **webview never calls the service itself** — the CSP (`connect-src ipc:`)
forbids it, and some payloads (microphone audio) can't originate in the webview
anyway. So new Tauri commands bridge it: `speak_text` / `stop_speaking` and
`transcribe_speech` / `stop_dictation` do the device-side audio I/O natively
(`crates/app/src/audio.rs`) and exchange WAV bytes with the plugin service.

Adding a new service capability therefore means: ship the plugin (manifest
`capabilities` + a `service`), and add the thin Tauri command(s) that locate it
via `active_service_plugin` and call it. `PluginInfo.capabilities` is already
surfaced to the UI, so a feature can light up its controls when a matching
plugin is enabled and `service` is `running`.

## 8. Capability-flag plugins (e.g. `model-browser`, `smart-model-router`)

A manifest's `python` block is optional (`Option<PythonRuntime>`), so a plugin
can be pure metadata: no venv, no setup, no service. `is_set_up()` returns
`true` immediately, `service` is always `notApplicable`, and `set_enabled`
just persists the enable flag and emits `Ready` — there is nothing to run.

This is a legitimate way to ship a **feature flag with a UI**: the plugin
exists only so the user can opt a whole app surface in/out from Add-ons,
without inventing a second toggle mechanism elsewhere (e.g. a Settings
checkbox). The `hf-browser` plugin (`plugins/hf-browser/`) is the first
example — enabling it reveals the "Browse" sidebar entry and view; all the
actual code (API client, commands, React view) lives in the main app, not in
the plugin directory.

Gating the UI on a python-less capability is **not** the §7 pattern — §7's
`provides()` requires the capability *and* a running `service`, which a
python-less plugin never has. The correct gate is capability + `enabled` only:

```ts
plugins.some((p) => p.capabilities.includes("model-browser") && p.enabled)
```

(`setup` is trivially `ready` for a python-less plugin the instant it's
enabled, so it doesn't need checking either.) When the UI keeps its own nav
state (e.g. a `view` the sidebar entry points at), redirect away from that
view if the capability disappears — the sidebar entry unmounts as soon as
`plugins` updates, but nothing else changes `view` for you.

Capability names elsewhere are single lowercase words (`chat`, `tts`, `stt`);
`model-browser` is intentionally kebab-case and two words for clarity — the
framework treats capabilities as free-form strings, so either style works.

A capability-flag plugin doesn't have to be purely UI-facing. `smart-model-router`
(`plugins/smart-model-router/`, multi-model auto-pick) is the same shape as
`model-browser` — no `python` block, `is_set_up()`/`service` behave
identically — but its capability also gates a **backend** decision, not
just a UI reveal: `commands::classify_message` needs to know whether it's
enabled even though request-triage's classifier itself lives entirely in
Rust, with nothing for the TS-only `plugins.some(...)` check above to gate.
For that, use `PluginManager::capability_enabled(capability) -> bool`
(`crates/core/src/plugin.rs`) — the same capability-and-enabled check as
above, callable from Rust:

```rust
state.plugins.capability_enabled("smart-model-router").await
```

Add both checks (the TS predicate for anything the UI itself needs to gate,
`capability_enabled` for anything a Rust command needs to gate) when a new
capability-flag plugin's effect isn't purely cosmetic.

## 9. Core plugins — self-provisioning, no Add-ons toggle

Every plugin above is an **opt-in add-on**: discovered but disabled until the
user flips it on from Settings → Add-ons. Some capabilities may not be
optional extras, though: one can need the exact same one-time Python setup +
managed-sidecar lifecycle as any other service plugin while "the user finds
it in Add-ons and enables it first" doesn't fit, because it's meant to feel
like a normal part of the app. No shipped plugin is core today; `web-search`
was until it became an experimental add-on.

A manifest sets `"core": true` for this. It changes exactly two things:

1. **Hidden from Add-ons.** `PluginManager::list()` (what `list_plugins`
   returns) filters core plugins out entirely — there's nothing to toggle, so
   there's nothing to show.
2. **Always "enabled."** Every enabled-state check
   (`active_service_plugin`, `active_chat_plugin`, `capability_enabled`)
   treats a core plugin as enabled unconditionally (unless it's also marked
   experimental and that switch is off, §1), ignoring whatever's actually
   persisted in `plugins.json` for it (there may be nothing, or a stale
   record from before it became core).

Nothing else changes: setup still creates a venv and runs `setup_plugin.py`,
the service still gets the exact engine lifecycle (free port → spawn →
health-poll → kill), and mutable state still lives under
`<app-data>/plugin-data/<id>/`. A core plugin just never had a `set_enabled`
call to trigger any of that — so instead of a Settings toggle, whichever
Rust call site actually needs the capability calls

```rust
PluginManager::ensure_capability_service(capability: &str, on_event: impl FnMut(PluginEvent)) -> Result<u16, String>
```

which finds the active plugin advertising `capability`, runs setup if it's a
core plugin that hasn't been set up yet (streaming the same `PluginEvent`s
Add-ons setup would have shown), starts its service, and returns the port —
self-provisioning on the very first real use instead of requiring a prior
visit to Settings. (An add-on providing the capability works through the
same call, but only once the user enabled it and its setup finished:
`set_enabled` owns an add-on's setup, and this never starts a second one.)
`web-search`'s callers use it this way today. Call it lazily
(only once the capability is actually about to be used, not speculatively —
setup can take tens of seconds the first time) and translate its
`PluginEvent`s into whatever progress signal the calling feature's own UI
understands (`web-search`'s callers use `PluginEvent::as_search_setup_chat_event`
to turn `Setup` events into `ChatEvent::SearchSetupProgress`, which the chat
UI already renders as a status line).

The user's actual consent gesture for a core plugin lives wherever that
capability is surfaced in the *feature's own* UI rather than in a global
Settings switch — "core" describes how the plugin is *provisioned*, not
whether using it needs the user to ask for it.
