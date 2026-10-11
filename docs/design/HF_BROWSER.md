# Hugging Face model browser — an embedded hub, plugin-gated

Plan produced 2026-07-11 (multi-agent research workflow: 5 codebase-seam agents +
1 live HF-API agent, then 4 adversarial verifiers re-checking every file:line
seam, the plugin-gating design, the live API claims, and plan completeness).

## Verdict

**Feasible entirely with existing plumbing — the only new backend work is three
small HF Hub API calls; everything downstream of "user picked a file" already
exists.** `add_hf_model` → `download_model` → `get_models` handles registration,
resumable sharded downloads, and library display unchanged. The webview cannot
embed huggingface.co (the site sends `x-frame-options: DENY`, verified live) and
the CSP blocks all webview network anyway, so this is a **native UI over the HF
JSON API**, with every request going through Rust commands — the same road
`list_hf_models` already takes.

The elephant: ARCHITECTURE.md explicitly decided **against** this feature for
beginners ("Curated catalog over model search. Beginners don't want a
HuggingFace browser"; "Custom models are an escape hatch, not a browser"). The
plan reconciles rather than reverses that decision: the browser ships as a
**built-in, python-less plugin** (`plugins/hf-browser/`, capability
`"model-browser"`, no `python` block — fully supported today, see seams). The
Browse view only appears when the plugin is enabled. Beginners never see it;
the catalog recommendation flow (`recommend()`, onboarding, "our pick") is
untouched. All real code lives in core/app/ui as usual — the plugin is the
opt-in switch, not a runtime.

Verification surfaced two concrete catches, both now explicit work items:

1. **A pre-existing release blocker** (fixed in Phase 0, independent of the
   browser): nothing bundles `plugins/` into packaged resources
   (tauri.conf.json bundle.resources maps only `resources/llama/`), so in a
   non-portable release build `resolve_plugin_dirs`' resource dirs are empty
   and *no* built-in plugin — web-search, speech, or the new hf-browser — is
   discovered. Phase 0 adds the bundle mapping and a packaged-build smoke
   test, and can ship on its own before any browser work.
2. **A compile-breaking API mismatch in the original fit-badge design** (fixed
   in decision 6 and Phase 3): `selection::fit_for` is a three-argument
   function — `fit_for(spec, hw, disk_offload)` at selection.rs:91, not the
   two-argument call the initial research assumed. Phase 3 threads
   `settings.disk_offload` explicitly and documents the transient-spec
   consequences (`moe: false` → `DiskSlow` unreachable).

Our differentiator over the real HF site: **fit badges**. Each quant in the
file picker gets the same `selection::fit_for` verdict the Library shows
("Fits comfortably" / "Tight" / "Too big"), computed from real LFS sizes before
the user commits to a 20 GB download — with honest "rough estimate" copy,
because the inputs are heuristics (see decision 6).

## Verified technical foundation (July 2026)

Sources: live huggingface.co API probes (2026-07-11, re-probed independently by
a second agent the same day), huggingface.co/docs/hub (api, rate-limits,
models-gated), `huggingface_hub` client docs. ✅ = adversarially confirmed
against the live API.

1. ✅ **List/search endpoint covers search, trending, and filters in one call.**
   `GET https://huggingface.co/api/models` with `search=`, `author=`,
   `filter=gguf` (canonical GGUF filter), `pipeline_tag=`,
   `sort=trendingScore|downloads|likes|lastModified|createdAt` (`sort=trending`
   400s), `direction=-1`, `limit=` (≤1000). Default items carry `id`, `likes`,
   `trendingScore`, `downloads` (last-30-days — label it), `tags[]` (including
   `license:*`), `pipeline_tag`, `createdAt`, and usually-but-not-always
   `library_name` (observed absent on some items — every DTO field must be
   `Option`).
2. ✅ **Pagination is cursor-only.** Next page comes from the
   `Link: <…&cursor=…>; rel="next"` response header; no total-count header is
   sent. Build infinite scroll, not numbered pages.
3. ✅ **`gated`/`lastModified`/`author` are absent from default list items.**
   They need `full=true` (heavy: includes `siblings`) or `expand[]=` (returns
   *only* expanded fields + `id` + `trendingScore`, silently overriding
   `full`). Use `expand[]` with exactly the card fields.
4. ✅ **Detail endpoint gives GGUF metadata for free.**
   `GET /api/models/{repo}` returns `gguf: {total (param count), architecture,
   context_length, chat_template, totalFileSize}` for GGUF repos, plus
   `cardData.license`, `downloads`, `likes`, `lastModified`, `tags`.
   `?blobs=true` upgrades `siblings` to real byte sizes + `lfs.sha256`.
   (The quant picker will instead reuse our existing tree-API client — same
   data, already tested for shard grouping.)
5. ✅ **`gated` is `false | "auto" | "manual"`** — a bool/string union, all
   three values observed live. Needs `#[serde(untagged)]`, never plain `bool`.
6. ✅ **READMEs fetch anonymously** at
   `https://huggingface.co/{repo}/raw/{rev}/README.md` (200 direct on the happy
   path; renamed repos can redirect, so the fetch needs a redirect policy —
   see decision 5). Leading `---`-delimited YAML frontmatter must be stripped;
   gated repos 401 on `/raw/` even though their metadata lists publicly.
7. ✅ **Rate limits: anonymous = 500 "api" requests / 3000 "resolver" requests
   per fixed 5-minute window per IP**, advertised via `RateLimit` /
   `RateLimit-Policy` headers; 429 on excess with `t=<seconds-to-reset>`.
   Debounced search browsing stays far below this.
8. ✅ **huggingface.co cannot be iframed** — pages send
   `x-frame-options: DENY`. "Embedded version of the site" must mean native UI
   over the JSON API.
9. **Gated repos can't be downloaded without a user token + prior license
   acceptance on hf.co.** OpenPal has no HF-token support today and the
   existing behavior (friendly "gated model" error on 401/403) stays; the
   browser badges gated models up front instead of letting users hit the error.

## What OpenPal's rules mean today (verified seams)

- **All HF traffic is Rust-side.** CSP `connect-src ipc: http://ipc.localhost`,
  `img-src 'self' data:` (crates/app/tauri.conf.json:26); comment at
  crates/app/src/commands.rs:557-559: "the only road to Hugging Face". Remote
  images are proxied as data: URIs (`fetch_favicon`, commands.rs:823) with the
  `is_hf_host` allowlist + per-hop redirect revalidation precedent
  (commands.rs:1430, 1460-1462) — or omitted. Note: the shared `state.http`
  client follows redirects blindly (comment at commands.rs:1453-1454), so
  fetches that may redirect need their own policy.
- **The per-repo half already exists.** `parse_input` (crates/core/src/huggingface.rs:40),
  `list_gguf_files` (huggingface.rs:99, tree API; gated 401/403 → friendly
  error :114-120; mmproj filtering :164-167; shard grouping :255),
  `quant_from_file_name` (:310), `estimate_params_b` (:345); commands
  `list_hf_models` (crates/app/src/commands.rs:561) and `add_hf_model` (:575 —
  re-fetches size/sha server-side at :586-588, never trusts the UI, dedupes by
  resolved URL and returns the existing id :599-604, persists via
  `custom::hf_spec` crates/core/src/custom.rs:86).
- **Downloads reuse `download_model`** (commands.rs:286): multi-shard, disk-space
  gate, one in-flight per model id, resumable `.part` + sha256, progress over
  `tauri::ipc::Channel<DownloadEvent>`; UI side `store.startDownload`
  (ui/src/state/store.ts:212). add_hf_model persists before returning the id,
  so add → `startDownload(returnedId)` works immediately (new wiring — the
  SettingsSheet flow discards the returned id today). No new download code.
- **Python-less plugins work today.** `PluginManifest.python` is `Option`
  (crates/core/src/plugin.rs:35-49); `is_set_up()` is immediately true
  (:749, test `python_less_plugins_need_no_setup` :961), service state is
  permanently `ServiceState::NotApplicable` (:197-201, serialized
  `"notApplicable"`), `set_enabled` just persists + emits `Ready`.
- **UI gating on capabilities is established but must be adapted.** The voice
  buttons (useVoiceReady, ui/src/views/ChatView.tsx:143-153) gate on
  capability + `enabled` + setup `"ready"` + **service `"running"`** — a
  python-less plugin never reaches "running", so the Browse gate is exactly
  `p.capabilities.includes("model-browser") && p.enabled` (no service check).
  PLUGINS.md §7 blesses capability-driven UI for *service* plugins; hf-browser
  is a deliberate adaptation, documented in PLUGINS.md as part of Phase 2.
- **Plugins load at boot, everywhere.** `store.loadPlugins` runs unconditionally
  at bootstrap (ui/src/state/store.ts:183) and `list_plugins`
  (commands.rs:1512) has no portable gate — so a Sidebar button gated on
  `store.plugins` appears on boot without new wiring.
- **Packaged builds don't ship `plugins/` today (pre-existing gap).**
  bundle.resources maps only `"resources/llama/": "llama/"`
  (crates/app/tauri.conf.json:39-41); the repo-path fallback in
  `resolve_plugin_dirs` is debug-only (crates/app/src/state.rs:306-307). In a
  release build no built-in plugin is discovered. Phase 2 fixes this for all
  plugins.
- **Portable mode:** built-in plugin dirs are skipped but `<data_dir>/plugins`
  is always scanned (state.rs:299-311); the Plugins tab is hidden in the UI
  (ui/src/views/Sidebar.tsx:93). Since plugins default to disabled
  (plugin.rs:231) and the enable switch lives only in the hidden Plugins tab,
  **drop-in alone is not enough in portable** — the user would also have to
  hand-edit `<data_dir>/plugins.json`. v1 accepts "browser unavailable in
  portable" (see risk register / open questions).
- **UI conventions:** state-based nav — `View` union (ui/src/state/store.ts:27),
  ternary in App.tsx:45-51, Sidebar buttons (Sidebar.tsx:87-103,
  portable-gating example at :93). Single Rust seam ui/src/lib/api.ts;
  hand-mirrored types in ui/src/lib/types.ts (camelCase, adjacently-tagged
  enums). Plain CSS with Hearth tokens; per-view stylesheet imported in
  main.tsx. Markdown rendering exists (ui/src/components/Markdown.tsx,
  react-markdown + remark-gfm). Sizes must use `totalModelSizeBytes`
  (ui/src/lib/format.ts:29 — its doc comment already anticipates "the Hugging
  Face browser's HfFile"). React 19 StrictMode double-mount guards required;
  the alive-flag cleanup pattern exists (Library.tsx:167-178,
  SettingsSheet.tsx:605-614).
- **Fit math:** `selection::fit_for(spec: &ModelSpec, hw: &HardwareInfo,
  disk_offload: bool) -> (FitLevel, String)` (crates/core/src/selection.rs:91).
  Command-layer callers read the flag via `state.settings.lock().await
  .disk_offload` (precedent: commands.rs:72-73). FitLevel is mirrored in TS and
  adding a variant costs ~8 touch points — we add none.
- **Errors** are friendly full sentences via `friendly()`/`internal()`
  (commands.rs:28/:52); `Result<T, String>` everywhere.
- **Offline stance:** network is user-initiated only, no telemetry; browsing
  may fail with "check your internet" copy but nothing else breaks.

## Design decisions (recommended)

1. **Plugin-gated core feature, not a plugin runtime.** Ship
   `plugins/hf-browser/plugin.json` — `id: "hf-browser"`, name "Model Browser",
   `capabilities: ["model-browser"]`, **no `python` block**. Enabling it in
   Plugins reveals a "Browse" sidebar entry. Rationale: honors the "plugin"
   framing, reconciles the ARCHITECTURE.md "no HF browser for beginners"
   decision (opt-in, invisible by default), and requires zero plugin-framework
   changes. Capability naming: `"model-browser"` is the first multi-word
   capability (existing: `chat`, `tts`, `stt`) — kept deliberately for clarity,
   with a one-line note in PLUGINS.md that capabilities are free-form strings
   and kebab-case is fine. Rejected alternatives: always-visible Library tab
   (reverses a documented decision); Settings→Advanced toggle (invents a second
   opt-in mechanism when plugins already are one); Python-service plugin
   (pointless — the HF API is plain REST reachable from `state.http`, and
   Python adds setup friction + portable exclusion for nothing).
2. **Gating is UI-only, and that's deliberate.** The new commands are read-only
   proxies to public HF endpoints, and the app already exposes ungated HF
   commands (`list_hf_models`/`add_hf_model` serve SettingsSheet regardless of
   any plugin). Enforcing plugin state in the new commands would make them the
   only plugin-checked commands in the app for no security gain. The plugin
   gates the *surface*, not the capability.
3. **Native UI over the JSON API** (forced: `x-frame-options: DENY` + CSP).
   New view `ui/src/views/Discover.tsx` + `styles/discover.css`; `View` union
   gains `"discover"`. When the model-browser capability disappears from the
   enabled-plugins list while `view === "discover"` (user disables the plugin
   mid-session), redirect to `"library"` — the Sidebar entry unmounts
   immediately via the optimistic patch in `setPluginEnabled`
   (store.ts:371-373), so the view must follow.
4. **Landing state = trending GGUF**: `filter=gguf&sort=trendingScore&direction=-1&limit=30`,
   infinite scroll on the Link cursor. `filter=gguf` makes dead ends **rare,
   not impossible** — a gguf-tagged repo can still yield an unusable picker
   (mmproj-only, incomplete shard sets, gated), so the model page needs an
   explicit "this repo has no files OpenPal can use" state that surfaces
   `list_gguf_files`' existing friendly errors (it already distinguishes
   gated / incomplete-shards / no-GGUF).
5. **Search results stay light**: use `expand[]` for exactly the card fields
   (downloads, likes, pipeline_tag, tags, gated, lastModified); derive license
   chip from the `license:*` tag; label downloads "last 30 days"; treat every
   field as optional. **Input hygiene and redirects:** validate
   `repo_id`/`revision` before URL interpolation (reuse `parse_input` or its
   charset rule — precedent commands.rs:565); fetch READMEs with either a
   no-redirect policy or `is_hf_host` per-hop revalidation (favicon/preview
   precedent), never the blindly-redirecting default client behavior.
6. **Fit badges are computed in the command layer**, not core: build a
   transient spec per file and call `fit_for(spec, &state.hardware,
   settings.disk_offload)` (selection.rs:91) — core stays hardware-agnostic,
   nothing persisted. Params: prefer `gguf.total` from the detail response
   (real param count, free on the page the picker sits on), falling back to
   `estimate_params_b` (a bits-per-weight heuristic). Badges carry hedged copy,
   reusing SettingsSheet's precedent ("our memory estimates for them are rough
   guesses", SettingsSheet.tsx:297-302). Returned as an optional decoration
   alongside HfFile so SettingsSheet's existing use is unaffected.
   **Superseded (2026-07-18):** transient specs no longer always carry
   `moe: false` — `get_hf_quant_picker` now range-fetches and parses the
   smallest file's own GGUF header (`crate::gguf_meta`, via
   `huggingface::fetch_gguf_header_meta`) for real MoE-ness and exact
   parameter counts, falling back to this section's estimate only when that
   fails. `DiskSlow` is also no longer gated on MoE at all — see
   `selection::fit_for`'s module docs ("let the user be the judge"). The
   search-grid badge (decision above this one) still uses the rough
   Q4_K_M-synthetic estimate and is now presented to the user as a binary
   "Could fit"/"Could not fit" hint, not the full 5-level badge.
7. **Terminal action is the existing pipeline, unchanged**: "Add to library" →
   `add_hf_model(repoId, revision, filePath)` → returned id →
   `store.startDownload(id)`. Browsed models are never `is_recommended`.
   **Honesty about what "added" means:** `hf_spec` caps added models at
   `context_max: 8192` while the model page shows `gguf.context_length` (often
   131072) — the page states plainly that OpenPal runs added models at 8k
   context (threading real context into `hf_spec` is an open question, since
   it changes KV-cache fit math).
8. **The two HF surfaces have distinct jobs, both stay.** The Models page's
   "Add your own model" (at its bottom, once Settings → AI model's "Add your
   own models" switch is on) remains the plugin-independent escape hatch and the
   only path for pasted direct-file links and local `.gguf` files; the browser
   is the discovery surface. Both render the same `list_hf_models` data — the
   fit-badge decoration can be shared so they never disagree. A discoverability
   link between them is deliberately deferred (open question).
9. **Gated models are shown, badged, and honest**: "This model requires
   accepting a license on huggingface.co — OpenPal can't download it yet."
   HF-token support is deliberately out of scope (future phase).
10. **No remote images in v1.** The app is text-first; README image links are
    dropped by the renderer (CSP blocks them anyway). A fetch-as-data-URI proxy
    (favicon pattern, size-capped) is a later polish option.
11. **Politeness and correctness by construction**: debounce search ≥400 ms;
    a **latest-wins sequence token** around every search/page/detail/README
    fetch (Tauri invokes aren't cancellable — a slow response for query A must
    not clobber query B's grid; stale cursor-appends are dropped); StrictMode-
    safe fetch-on-open from day one (alive-flag pattern, Library.tsx:167-178).
    A small `Mutex<HashMap>` page/detail cache on AppState (disk_speed_cache
    pattern); map 429s to "Hugging Face is asking us to slow down — try again
    in about N seconds" using the `RateLimit` header. No background/automatic
    requests; trending fetches only when the view opens. **No new crates** —
    hand-parse the `Link` and `RateLimit` headers (they're single-line
    formats); avoids touching the pinned `time = 0.3.47` constraint
    (ARCHITECTURE.md gotcha).

## Phases

Every phase closes out with `cargo test --workspace` and `graphify update .`
(repo convention).

### Phase 0 — Ship built-in plugins in packaged builds (pre-existing bug fix)

Standalone fix, no browser code involved — can be committed and released
before Phase 1. Today web-search and speech are only discoverable in debug
builds (repo-path fallback, state.rs:306-307) because nothing stages
`plugins/` into bundle resources.

- Add a bundle.resources mapping so `plugins/` lands at
  `resource_dir/plugins` (matching `resolve_plugin_dirs`' lookup at
  state.rs:303), filtered of `__pycache__`/`tests` — via tauri.conf.json or a
  bundler staging step, whichever handles exclusions cleanly. Apply the same
  treatment to the portable config only if built-ins are ever wanted there
  (today portable deliberately skips them — leave as is).
- Tests: packaged-build smoke test — `bundler build`, open the app, confirm
  the Plugins tab lists web-search and speech (and hf-browser once Phase 2
  lands); confirm enabling web-search still runs its setup from the bundled
  resource dir (read-only plugin dir, mutable state in plugin-data — the
  existing contract).

### Phase 1 — Core HF hub client (pure Rust, no UI)

- `crates/core/src/huggingface.rs` gains: `search_models(client, &HfSearchQuery)
  -> Result<HfSearchPage>` (builds `/api/models` query incl. `expand[]`, parses
  the Link header into `next_cursor`), `model_details(client, repo_id) ->
  Result<HfModelDetails>`, `fetch_readme(client, repo_id, revision) ->
  Result<String>` (frontmatter strip, 1 MiB cap, 401→gated copy, 404→"no model
  card", no-redirect or per-hop `is_hf_host` revalidation).
- Repo-id/revision validation helper shared by all three (parse_input charset
  rule) — inputs are interpolated into URLs.
- New DTOs in `crates/core/src/types.rs` (serde camelCase, tolerant: unknown
  fields ignored, every field `Option` that can be): `HfSearchQuery`,
  `HfModelSummary`, `HfSearchPage { models, next_cursor }`, `HfModelDetails`,
  gated as `#[serde(untagged)]` bool|string normalized to `{ gated, mode }`.
- Error mapping mirrors `list_gguf_files`: offline, 404, 429 (parse
  `RateLimit` `t=`), 5xx — all friendly sentences.
- Tests: unit tests on canned JSON fixtures (list item with/without expand and
  with/without `library_name`, gated `false`/`"auto"`/`"manual"`, Link-header
  cursor extraction, frontmatter stripping incl. no-frontmatter and
  frontmatter-only files, repo-id validation rejects path escapes); one
  `#[ignore]`d live test hitting `/api/models?filter=gguf&limit=3` (pattern:
  `lists_a_real_repo`, huggingface.rs:484).

### Phase 2 — Commands, plugin flag, Browse view skeleton

- Commands in `crates/app/src/commands.rs` next to `list_hf_models`:
  `search_hf_hub(state, query) -> Result<HfSearchPage, String>`,
  `get_hf_model_details(state, repo_id)`, `get_hf_model_readme(state, repo_id,
  revision)`; registered in crates/app/src/lib.rs `generate_handler!`; typed
  wrappers in ui/src/lib/api.ts; TS mirrors in ui/src/lib/types.ts.
- `plugins/hf-browser/plugin.json` (python-less, capability
  `"model-browser"`) + short README.md; manifest unit test alongside
  `parses_the_web_search_manifest`; PLUGINS.md note on python-less
  capability-flag plugins and kebab-case capability names.
- UI: `View` union + App.tsx branch + Sidebar "Browse" button with the exact
  gate `plugins.some(p => p.capabilities.includes("model-browser") &&
  p.enabled)` (no service/setup check — python-less plugins are `"ready"` with
  service `"notApplicable"`); view fallback `"discover"` → `"library"` when the
  capability disappears. Discover view v1: trending grid + debounced search box
  + sort select, latest-wins sequence token on every fetch, StrictMode-safe
  mount, result cards (name, author, downloads, likes, license chip, gated
  badge, pipeline tag), infinite scroll, loading/empty/error states.
- Tests: `cargo test --workspace` (manifest, DTO fixtures); manual `npm run
  dev` pass: enable plugin → Browse appears; disable while on Discover →
  redirected to Library; re-run the Phase 0 packaged-build smoke test and
  confirm hf-browser now appears alongside web-search and speech; portable
  build hides Browse.

### Phase 3 — Model page + add/download funnel

- Drill-in model page inside the Discover view: metadata header
  (`gguf.total` params, architecture, context length with the "runs at 8k in
  OpenPal" honesty line, license, gated), lazily-fetched README (Markdown
  component, images dropped), quant picker from `list_hf_models` decorated
  with **fit badges**.
- Fit-decoration helper (explicit fix from verification — the original design
  assumed a 2-arg `fit_for` and would not compile): command layer builds a
  transient spec per file (params from `gguf.total` when the detail response
  has it, else `estimate_params_b`) and calls the real 3-arg signature
  `selection::fit_for(&spec, &state.hardware, disk_offload)`
  (selection.rs:91), with `disk_offload` read via
  `state.settings.lock().await.disk_offload` (precedent commands.rs:72-73).
  Transient specs are `moe: false`, so `DiskSlow` is unreachable — same as
  user-added models today; badges carry hedged "rough estimate" copy; sizes
  via `totalModelSizeBytes`.
- Unusable-repo states surfacing `list_gguf_files`' friendly errors: gated
  (badge + explanation, no picker), incomplete shard sets, no GGUF files.
- "Add to library" → `add_hf_model` → `startDownload(returnedId)` with the
  standard progress UI; "✓ In your library" state for already-added files
  (URL dedupe returns the existing id, commands.rs:599-604).
- Tests: unit test for the fit-decoration helper (incl. disk_offload flag
  threading and gguf.total fallback); manual e2e: browse → open card → pick
  quant → download → model runs in Chat; re-add same file is a no-op; gated
  repo shows badge and no download path; repo with no usable files shows the
  friendly state.

### Phase 4 — Polish

- Page/detail cache on AppState (single-flight, short TTL); 429 retry copy;
  author + `pipeline_tag` filter chips (**text-generation only in v1** — no
  vision chip: the add funnel strips mmproj and hard-codes `vision: false`
  (custom.rs:154-160), so advertising vision models would be a trap until the
  funnel supports them, see future phases); "Open on huggingface.co" via
  `api.openLink`; copy pass to the app's friendly voice; keyboard/scroll
  ergonomics (view owns its scroll container).
- Tests: cache single-flight unit test; rate-limit copy unit test; manual
  scroll-restore + StrictMode double-mount check.

### Phase 5 (future, separate decision) — HF token for gated models

- Settings field (`force_cpu` end-to-end template), `Authorization: Bearer`
  attached **only** to huggingface.co/hf.co requests, gated download flow with
  "accept the license on hf.co first" hand-off, higher rate limits as a bonus.
  Not in scope now.

### Phase 6 (future, separate decision) — Vision models through the funnel

- Stop filtering mmproj siblings in `list_gguf_files` (pair them per HfFile
  instead), populate `mmproj_url`/`vision` in `hf_spec`, then add the vision
  filter chip. Until then browsed vision repos install as text-only, which is
  today's behavior for the SettingsSheet path too.

## Risk register

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Unversioned API: `/api/models` list behavior is documented mainly via the `huggingface_hub` client and can drift | Medium | Tolerant serde (ignore unknown fields, `Option` everywhere), `#[ignore]`d live test, graceful degradation (missing field → hide chip, never crash) |
| `expand[]` quirks: silently overrides `full=true`, drops non-expanded fields | Low | Fixture-driven tests pin the exact param set; fall back to `full=true` if `expand` proves unreliable |
| Rate limiting (500 api-req/5 min anon, per IP) | Low | Debounce, cache, fetch-on-open only, friendly 429 copy with reset time |
| Packaged builds ship no plugins today (pre-existing) — feature invisible in release if unfixed | **High** | Phase 0 bundle mapping + packaged-build smoke test (standalone, shippable before any browser work) |
| Stale-response races (uncancellable invokes) corrupt the grid | Medium | Latest-wins sequence token on every fetch from Phase 2 |
| Gated-model dead end frustrates users until Phase 5 | Medium | Badge early (from search results), honest copy, "Open on huggingface.co" link |
| Fit badges oversell precision (heuristic params, `moe: false`, 8k context cap vs displayed context) | Medium | `gguf.total` when available, hedged copy, explicit 8k-context line on the page |
| README rendering hazards (huge files, raw HTML, images, redirects on renamed repos) | Low | 1 MiB cap in Rust, react-markdown no-raw-HTML default, images dropped, no-redirect/allowlisted fetch |
| Scope creep vs. curated-catalog philosophy | Medium | Opt-in plugin gate; no changes to `recommend()`, onboarding, or Library defaults; browsed models never auto-selected except the existing first-install rule |
| Portable mode has no path to enable the plugin (tab hidden, drop-in still defaults disabled) | Low | Accepted for v1: browser unavailable in portable; revisit if asked (open question) |

## Open questions

- Naming/placement: "Browse" sidebar entry (recommended) vs. a tab inside
  Library? Display name "Model Browser" vs. "Hugging Face"?
- Discoverability: should Settings → Advanced ("Add your own model") show a
  quiet pointer to the browser plugin, or stay fully word-of-mouth?
  (Deliberately deferred out of Phase 4.)
- Context honesty vs. capability: thread `gguf.context_length` into `hf_spec`
  (changes KV-cache fit math and memory guardrails) or keep the 8192 cap with
  honest copy (current plan)?
- Portable: is "unavailable in portable" acceptable long-term, or does portable
  deserve an enable path (e.g. show the Plugins tab when user-dropped plugins
  are discovered)?
