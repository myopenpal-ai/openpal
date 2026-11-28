## Design philosophy (read before proposing features)

OpenPal is intentionally opinionated: it exists to get non-technical users
onto local-first LLMs, not to maximize configurability. Full rationale in
[ARCHITECTURE.md](ARCHITECTURE.md#design-philosophy). Two rules override
"more efficient" or "more powerful" suggestions unless the user explicitly
asks otherwise:

- **Beginner-first, power-user-second.** The default UX must stay usable by
  a total beginner. Advanced capability is welcome, but gate it behind
  Settings → Advanced or the plugin system rather than adding new
  default-path complexity.
- **Mono-model first.** One loaded model handles every task (chat, agent,
  web search). Don't propose routing to a second/specialized model (router,
  reranker, embedding model, etc.) as a default-path improvement, even if it
  would be measurably more efficient — that belongs behind an explicit
  opt-in, if at all.

## Project layout

| Path | Crate / package | Role |
| --- | --- | --- |
| `crates/core` | `openpal-core` | All logic, zero GUI: hardware scan, catalog and fit, resumable downloads, llama-server lifecycle and streaming, sync, agent loop, prompt-injection classifier. |
| `crates/app` | `openpal-app` | Tauri shell: thin commands over core, window config, capabilities. |
| `bundler/` | `openpal-bundler` | Engine fetch/stage, icons, build, sign, notarize. Never imported by the app. |
| `crates/relay` | `openpal-relay` | Store-and-forward sync relay binary (axum + SQLite, opaque blobs only). |
| `crates/sync-protocol` | `openpal-sync-protocol` | Shared wire envelope serde types for client ↔ relay. |
| `ui/` | `openpal-ui` | React + TypeScript (Vite). Talks to Rust only through `ui/src/lib/api.ts`. |
| `plugins/` | Python sidecars | `plugin.json` contract in [plugins/PLUGINS.md](plugins/PLUGINS.md). |

Keep the boundaries: core stays GUI-free, `crates/app` stays a thin shell, and
the relay and client share `crates/sync-protocol` so their wire formats cannot
drift silently.

## Repo rules that bite

- **Rust ↔ TypeScript types are a hand mirror.** `ui/src/lib/types.ts` mirrors
  `crates/core/src/types.rs` and no test checks parity — change both.
- **`ui/src/lib/api.ts` is the only seam between the UI and Rust.** A new Tauri
  command must be added there to be reachable from the frontend.
- **Every user-facing string needs both an `en` and an `fr` key, in every
  namespace pair** — `en.json`/`fr.json` plus `.injection`, `.recovery`,
  `.remoteAccess`, `.remoteClient` and `.remoteInference`.
  `ui/src/i18n/keys.test.ts` fails CI otherwise.
- **Tailwind only works inside `ui/src/components/ui/`.** The shadcn/ui
  components are vendored there, and `ui/src/styles/tailwind.css` scans only
  that folder, ships no preflight and maps shadcn colours onto `global.css`
  tokens — use tokens, never `dark:`. Its output is layered and loses to every
  unlayered rule, so layout and margins stay in the hand-written CSS.
- **Never run a blanket `cargo update`.** `time` is pinned in `Cargo.lock`; bump
  only the dependency you mean to.
- **Node 22 plus the declared npm line, or the lockfiles break.** Use the Node
  in `.nvmrc` and `corepack enable` for the npm in `package.json` (`10.9.9`);
  install with `npm run setup`, which reads the committed lockfiles. A different
  npm major rewrites `package-lock.json` in a shape CI's `npm ci` rejects.
- **Commits on `main` must be signed.** The ruleset enforces
  `required_signatures`, so an unsigned commit blocks the merge queue. Configure
  SSH signing once — see
  [CONTRIBUTING.md](CONTRIBUTING.md#commit-signing-required).
- **A user-facing change bumps the version in the same PR.** Run
  `node scripts/bump-version.mjs --bump patch|minor|major --notes "…"`. Changes
  confined to `docs/`, `*.md`, `.github/` or `scripts/` are exempt, and
  `[no release]` in the title or the `no-release` label opts out. CI applies the
  bump to in-repo branches; forks must run it themselves.
- **Third-party actions must be pinned to a full 40-char commit SHA.** The
  `action pins (SHA)` job fails the PR otherwise — see
  [docs/SECURITY_POSTURE.md](docs/SECURITY_POSTURE.md).
- **Platforms:** PR CI compiles Linux only, and maintainers test macOS on Apple
  Silicon. Platform-gated code (macOS sandbox, keychain, Windows paths, Android)
  isn't built by the normal gates — say which platform you verified it on. The
  exceptions: `notify-platforms.yml` builds and tests `crates/app/src/notify/`
  on macOS and Windows and type-checks it for Android; `agent-sandbox-e2e.yml`
  runs the real macOS Seatbelt tests under `crates/core/src/agent/`.
- **Verify UI changes in the real Tauri window.** `ui/src/lib/api.ts` calls
  `invoke` directly, so `vite dev` in a browser has no IPC at all. Use the
  `/verify` skill before claiming a UI or engine change works.

## Checks (what CI runs)

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p openpal-core && cargo test -p openpal-app && cargo test -p openpal-bundler && cargo test -p openpal-sync-protocol
cd ui && npm run build && npm run lint && npm test && npm run test:e2e
```

**Required on `main` (12 checks):** `rust-clippy (fmt, clippy)`; `rust-test
(openpal-core)`, `rust-test (openpal-app)`, `rust-test (openpal-bundler)`,
`rust-test (openpal-sync-protocol)`; `frontend (typecheck, unit, mocked e2e)`;
`version-bump (release gate)`; `speech`; `engine-e2e`; and `notify on macos
(clippy, tests)`, `notify on windows (clippy, tests)`, `notify on android
(clippy)`. The path-filtered ones report success when their paths don't change.

**Runs but is not required:** `action pins (SHA)`, `injection-e2e`,
`agent-sandbox-e2e` — a red one is still yours to fix.

**Also run locally when the diff can reach it:**

- `cargo test --workspace` — `crates/relay` has no CI test job, so this is the
  only run that exercises it.
- `cargo test -p openpal-core -- --ignored` — every `#[ignore]`d suite: the
  real llama-server runs (`engine-e2e`), the live Hugging Face checks, and the
  prompt-injection corpus (models download).
- `cargo test -p openpal-core --test prompt_injection_corpus -- --ignored --nocapture`
  — just the real classifier (`injection-e2e`); it downloads a ~300MB model.
- `npm run setup` and `cargo run -p openpal-bundler -- doctor` on a fresh clone.

[docs/TESTING.md](docs/TESTING.md) has the full test strategy.

## Tickets and pull requests

Every change is linked to a GitHub issue, and the PR body says `Closes #N`.
[docs/TICKETS.md](docs/TICKETS.md) is the ticket contract: four types
(`type/feature`, `type/bug`, `type/chore`, `type/spike`) with frozen `###`
headings. Use `/new-ticket` to file one and `/work-ticket` to implement or
resume one; in this repo `/work-ticket` replaces the generic `ship-feature`
workflow.

Branches are `<prefix>/<issue-number>-<slug>`: `feature/`, `bugfix/` (or
`hotfix/` for `priority/p0`), `chore/` (or `docs/`, `ci/`, `refactor/` when
clearer), `spike/`. Commits follow Conventional Commits. The PR checklist is in
[CONTRIBUTING.md](CONTRIBUTING.md#before-opening-a-pull-request).

### Merging: the merge queue on `main`

`main` is protected and merges through a **merge queue** (ruleset
`main: required checks + merge queue`, squash-only). How it interacts with
pushing and merging PRs:

- **No manual rebase is needed** when an earlier PR merges while yours is
  open. Merging a PR does *not* invalidate the others: the queue re-tests
  each queued PR against the latest `main` (and the other queued PRs) and
  merges them once green, in order. Don't rebase just "to be up to date" —
  only rebase to resolve a *real* conflict.
- **To merge a PR**: either click **Merge when ready** in the PR once checks
  pass (it joins the queue immediately), or enable **auto-merge** so green PRs
  queue by themselves. This is a manual, per-PR action — enabling auto-merge
  on `main` is a repo setting, not something that applies to every PR.
- If a queued PR ends up failing its checks against latest `main` (e.g. a real
  conflict with another queued PR), it is removed from the queue and stays
  open; fix the branch (rebase/merge `main`) and queue it again.
- Head branches are deleted automatically after merge. Since history is
  squash-only, force-pushing a stray fix to a merged branch is expected and
  harmless (`force-with-lease`).
- CI: every workflow whose checks gate `main` must also fire on `merge_group`
  events (the queue runs required checks on a throwaway `gh-readonly-queue/`
  branch). If you add a new gating workflow, add `merge_group:` to its
  triggers or the queue will stall waiting for checks that never run.
