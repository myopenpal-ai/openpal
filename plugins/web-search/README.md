# Web Search plugin

Lets your OpenPal assistant search the web and cite its sources. The search
loop itself (decide → search → fetch → rerank → cite) is native Rust —
`crates/core/src/agent/builtin/websearch.rs` (the `web_search`/`fetch_url`
tools) and `rerank.rs` (BM25 relevance scoring), driven by the same
`run_agent_turn` loop agent mode uses. This plugin's only remaining job is
running the **local search backend** those tools query.

Web search is an **experimental add-on** (`plugin.json`'s
`"experimental": true`, `PLUGINS.md` §1): it's off by default and only shows
on the Plugins screen while Settings → Advanced → Experimental features is on.
Turning it on there downloads a pinned SearXNG source, installs its
dependencies into this plugin's own Python environment, and starts it on a
loopback-only port — **no Docker, no commands, nothing to install by hand.**
The composer's web-search globe then appears and turns search on for the
current conversation (a full-agent-mode turn can also reach it through the
`"web-research"` `spawn_subagent` helper). The service keeps running for the
rest of the session (it isn't stopped when you turn the composer toggle back
off, only when you turn the add-on or Experimental features off, or on app
exit).

**"Private, local" describes the SearXNG process, not the search itself.**
Turning web search on for a chat means every search query (composed by your
local model, which may paraphrase earlier turns of the conversation) is
forwarded by that local SearXNG instance to real public search engines —
DuckDuckGo, Bing, Brave, Wikipedia, and Startpage are enabled by default (see
`setup_plugin.py`). Fetched page URLs (via `read_url`) go straight to
whatever third-party site they point at. Nothing leaves your computer unless
you turn the add-on on *and* turn web search on for that conversation with
the composer's globe.

**License note:** SearXNG is licensed [AGPL-3.0](https://github.com/searxng/searxng/blob/master/LICENSE),
not MIT like the rest of OpenPal. It is downloaded and run as a separate local
process only once you actually turn the add-on on — OpenPal's own
build never statically links or redistributes it — but you should know it's
there. See "Pinned SearXNG ref" below for the exact source pulled.

See `PLUGINS.md` (repo root `plugins/`) for the framework contract, and its
§1 for what `"experimental": true` changes.

## How it works

The framework drives two lifecycle scripts (both run inside the plugin's venv):

| script            | role | when |
|-------------------|------|------|
| `setup_plugin.py` | one-time install: download + extract pinned SearXNG, `pip install` its deps, generate `settings.yml` | when the add-on is turned on (idempotent; the Plugins card offers Retry if it fails) |
| `service.py`      | long-lived sidecar: runs SearXNG from source on `127.0.0.1:<port>` | from turning it on (or its first use after a restart) until it's turned off or the app exits |

There's no per-turn Python script anymore — the Rust `web_search`/`fetch_url`
tools call this service's JSON API directly over HTTP, resolved via
`PluginManager::ensure_capability_service("search", ...)` (see
`agent_bridge::build_web_search_deps` for the plain-chat path and
`SpawnSubagentTool::ensure_search_service` for full agent mode's
`"web-research"` helper).

### The managed SearXNG sidecar

`service.py` does **not** rely on SearXNG being pip-importable as a package.
Instead it adds the downloaded source dir to `sys.path`, points
`SEARXNG_SETTINGS_PATH` at the generated `settings.yml`, imports `searx.webapp`,
and serves its Flask app with werkzeug on `127.0.0.1:<port>` (`threaded=True`),
**loopback only**. It handles `SIGTERM` so the framework's stop is clean.

`web_search_chat_turn`/`SpawnSubagentTool` resolve this service's base URL via
`PluginManager::ensure_capability_service("search", ...)` — which starts the
service if it isn't running (and refuses while the add-on is off or still
setting up) — then the Rust `WebSearchTool` calls
`GET <base>/search?...&format=json` directly.

## Where data lives

The plugin directory is treated **read-only**. All mutable state lives in the
plugin's data dir (`<app-data>/plugin-data/web-search/`, passed as `--data-dir`
and `OPENPAL_PLUGIN_DATA`):

```
<plugin-data>/
  venv/                       # python venv the framework creates
  searxng-src/                # extracted pinned SearXNG source (top dir stripped)
    .openpal_searxng_ref      # records the ref we extracted (idempotency marker)
    .openpal_searxng_deps_ok  # records that deps were pip-installed for that ref
  settings.yml                # generated SearXNG config (json format enabled, etc.)
  setup.log                   # setup_plugin.py stderr
  service.log                 # service.py stderr
```

Nothing in the data dir is committed to git. Deleting `settings.yml` regenerates
it (with a fresh secret key) on the next setup; deleting `searxng-src/` triggers
a fresh download.

## Pinned SearXNG ref

SearXNG no longer publishes git tags or GitHub Releases — it ships a rolling
release off `master`. So this plugin pins an **immutable commit SHA**:

```
searxng/searxng @ cf1410af8d794793c8af02b97d3e1cf3bc7a7dda
  (master HEAD on 2026-06-15)
tarball: https://github.com/searxng/searxng/archive/cf1410af8d794793c8af02b97d3e1cf3bc7a7dda.tar.gz
```

To bump it: pick a newer commit from
<https://github.com/searxng/searxng/commits/master>, update `SEARXNG_REF` in
`setup_plugin.py` (and this README), and re-verify (below).

The generated `settings.yml` uses `use_default_settings: true`, so it only
overrides what OpenPal needs (notably `search.formats: [html, json]`, which is
**required** for the JSON API; `server.limiter: false`; `public_instance:
false`; `image_proxy: false`; `safe_search: 0`) and inherits everything else
from the pinned version's defaults.

## Reranking

Chunk relevance scoring is BM25 (`crates/core/src/agent/builtin/rerank.rs`),
not a cross-encoder — OpenPal loads one model and uses it for everything
("mono-model first"; see root `AGENTS.md`), so a second neural model just for
reranking is deliberately out of scope here. `fetch_url` reranks only when
called with a `query` argument and the page is long enough to need excerpting.

## Verifying SearXNG end-to-end (manual)

```sh
# 1. venv (no per-plugin deps needed anymore — service.py is stdlib-only)
python3 -m venv /tmp/ws/venv

# 2. one-time setup (downloads + installs SearXNG; streams NDJSON progress)
mkdir -p /tmp/ws/data
OPENPAL_PLUGIN_DATA=/tmp/ws/data /tmp/ws/venv/bin/python setup_plugin.py --data-dir /tmp/ws/data

# 3. run the service, then query the JSON API
/tmp/ws/venv/bin/python service.py --port 8888 --data-dir /tmp/ws/data &   # serves 127.0.0.1:8888
curl http://127.0.0.1:8888/healthz                      # -> OK (200)
curl 'http://127.0.0.1:8888/search?q=hello&format=json' # -> {"results":[ ... ]}
kill %1                                                  # clean SIGTERM shutdown
```

The health path is **`/healthz`** (SearXNG serves it; returns `OK`/200
instantly), which is what `plugin.json`'s `serviceHealthPath` points at.
