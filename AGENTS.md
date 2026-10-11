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
- **Never run a blanket `cargo update`.** `time` is pinned in `Cargo.lock`; bump
  only the dependency you mean to.
- **Pull-request CI compiles Linux only.** Platform-gated code (macOS sandbox,
  keychain, Windows paths, Android) isn't built by the PR gates — say which
  platform you verified it on.

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
