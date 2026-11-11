# Contributing to OpenPal

Thanks for your interest! OpenPal is a small project, so a little coordination
up front saves everyone time.

## Before you start

- **Read the [design philosophy](ARCHITECTURE.md#design-philosophy).** OpenPal
  is beginner-first and uses one model for everything. Proposals that add
  default-path complexity or a second model are usually declined; the same
  capability behind Settings → Advanced or a plugin is often welcome.
- **Open an issue before writing code** for anything bigger than a typo, so we
  can agree on the shape first. Use one of the four issue forms
  (Issues → New issue): feature, bug, chore/tech debt, or spike/research.
- **Platform reality:** only macOS on Apple Silicon is tested today. Bug
  reports and fixes from Windows and Linux users are especially valuable.

## Setting up

Development setup, running the app and the test commands are in the
[README](README.md#development). Check your environment with:

```bash
cargo run -p openpal-bundler -- doctor
```

## Working on a change

**Branch** from `main` using the issue type and number,
`<prefix>/<issue-number>-<short-slug>`:

| Issue | Prefix |
| --- | --- |
| Feature | `feature/` |
| Bug | `bugfix/` (or `hotfix/` for a critical fix) |
| Chore / tech debt | `chore/` (or `docs/`, `ci/`, `refactor/` when clearer) |
| Spike / research | `spike/` |

Example: `feature/47-resume-interrupted-model-download`.

**Commits** follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix(engine):`, `docs:` …).

**Repo rules that bite:**

- `ui/src/lib/types.ts` hand-mirrors `crates/core/src/types.rs`; no test checks
  parity, so change both.
- `ui/src/lib/api.ts` is the only seam between the UI and Rust; a new Tauri
  command has to be added there.
- Every user-facing string needs both an `en` and an `fr` key
  (`ui/src/i18n/keys.test.ts` fails CI otherwise). If you don't speak French,
  say so in the PR and a maintainer will help.
- Don't run a blanket `cargo update`; bump only the dependency you mean to.

## Before opening a pull request

Run the same checks CI runs:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p openpal-core && cargo test -p openpal-app && cargo test -p openpal-bundler
cd ui && npm run build && npm run lint && npm test && npm run test:e2e
```

Pull-request CI compiles on Linux only. If your change touches
platform-specific code (macOS sandbox, keychain, Windows paths, Android), say
which platform you built and tested it on. The notification code
(`crates/app/src/notify/`) is the exception: `notify-platforms.yml` also builds
it on macOS, Windows and Android when it changes. [docs/TESTING.md](docs/TESTING.md)
describes the full test strategy.

Then open the PR with `Closes #<issue>` at the top of the description (the
template has the slot) and fill in how you verified it. Say plainly if a check
was skipped or couldn't run.

Merging is **squash-only**: every PR lands on `main` as exactly one commit
carrying its Conventional-Commits title, and the head branch is deleted
automatically. `main` is protected — direct pushes are rejected, force-pushes
and branch deletion are blocked, and a PR cannot merge until its required
checks pass: the always-on `ci.yml` gate plus the path-filtered `plugin-tests`,
`engine-e2e` and `notify-platforms` checks (auto-satisfied when their paths
don't change). The exact check names are in
[docs/TESTING.md](docs/TESTING.md#branch-strategy-and-required-checks).

## Where things are

- [ARCHITECTURE.md](ARCHITECTURE.md) — how the app is put together.
- [docs/](docs/) — reference docs for current behavior (agent mode, remote
  access, sync pairing protocol, relay self-hosting, testing).
- [docs/design/](docs/design/) — design records: the plans and research behind
  features, some implemented and some still roadmap.
- [plugins/PLUGINS.md](plugins/PLUGINS.md) — the plugin contract.

## Security

Please don't report vulnerabilities in public issues — see
[SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE), the same as the project.
