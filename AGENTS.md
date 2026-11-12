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

## Repo rules that bite

- **Rust ↔ TypeScript types are a hand mirror.** `ui/src/lib/types.ts` mirrors
  `crates/core/src/types.rs` and no test checks parity — change both.
- **`ui/src/lib/api.ts` is the only seam between the UI and Rust.** A new Tauri
  command must be added there to be reachable from the frontend.
- **Every user-facing string needs both `en` and `fr` keys.**
  `ui/src/i18n/keys.test.ts` fails CI otherwise.
- **Tailwind only works inside `ui/src/components/ui/`.** The shadcn/ui
  components are vendored there, and `ui/src/styles/tailwind.css` scans only
  that folder, ships no preflight and maps shadcn colours onto `global.css`
  tokens — use tokens, never `dark:`. Its output is layered and loses to every
  unlayered rule, so layout and margins stay in the hand-written CSS.
- **Never run a blanket `cargo update`.** `time` is pinned in `Cargo.lock`; bump
  only the dependency you mean to.
- **Pull-request CI compiles Linux only.** Platform-gated code (macOS sandbox,
  keychain, Windows paths, Android) isn't built by the PR gates — say which
  platform you verified it on. The one exception is the notification code
  (`crates/app/src/notify/`): `notify-platforms.yml` builds and tests it on
  macOS and Windows, and type-checks it for Android, when it changes.

## Checks (what CI runs)

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p openpal-core && cargo test -p openpal-app && cargo test -p openpal-bundler
cd ui && npm run build && npm run lint && npm test && npm run test:e2e
```

[docs/TESTING.md](docs/TESTING.md) has the full test strategy.

## Tickets and pull requests

Every change is linked to a GitHub issue, and the PR body says `Closes #N`.
Branch names and the PR checklist are in [CONTRIBUTING.md](CONTRIBUTING.md).

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
