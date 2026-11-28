# Security posture (repo, CI, org)

This page is the single source of truth for OpenPal's repo/CI security
invariants. PRs and settings changes that break one of these are a regression
even if CI is green. It complements [SECURITY.md](../SECURITY.md) (vulnerability
reporting and product threat model) and [RELEASING.md](RELEASING.md) (release
runbook).

Last reviewed: 2026-11-20 (hosted runner inventory and the self-hosted runner rule).

---

## 1. Machine-enforced invariants

These are enforced by GitHub settings; none of them can be silently dropped by a
normal PR.

| Invariant | Where enforced | Since |
| --- | --- | --- |
| `main` is PR-only, squash-only, no force-push, no deletion | ruleset `main: required checks + merge queue`, **no bypass actors** | 2026-11-12 |
| 12 required status checks + merge queue on `main` | same ruleset (`required_status_checks`, `merge_queue`) | 2026-11-12 |
| Every commit merged to `main` is **signed and verified** | same ruleset, `required_signatures` rule | 2026-11-15 |
| Release/nightly tags are **signed**, can't be moved or deleted | tag ruleset `v*`/`b*`: `required_signatures`, `non_fast_forward`, `deletion` | 2026-11-15 |
| Tags pushed from CI are **signed in the runner** with a dedicated key, never a personal one | `rolling.yml` / `make-release.yml` import `GIT_SIGNING_KEY` and `git tag -s`; public half registered as an SSH signing key on the operator account | 2026-11-15 |
| A push is only green once its downstream run/check **actually started**: tag and auto-bump pushes verify the cascade instead of trusting the push's exit code (a `GITHUB_TOKEN` push starts no workflow and still exits 0 — issue #131) | `make-release.yml` / `rolling.yml` wait for the `portable` run on the new tag; `version-bump.yml` waits for its re-triggered check on the new PR head; all three fail red naming the expected run | 2026-11-21 |
| All third-party actions pinned to a full 40-char commit SHA | CI gate `action-pins` in `ci.yml` (fails any floating `@tag` ref) | 2026-11-15 |
| Least-privilege `GITHUB_TOKEN`: `contents: read` default, `actions: write` only on the `frontend` job, `contents: write` + `deployments: write` only on `portable`'s `publish`/`smoke`/`discard` (`smoke` needs write to read the *draft* release it verifies — drafts 404 for read-only tokens), `engine-update`'s `publish`, and `publish-update-feeds` (`promote` needs `contents: write` to edit the release; its `engine` job needs `contents: write` + `deployments: write`); `actions: read` only on the tag-pushing workflows and `checks: read` only on `version-bump`, both for their cascade verification | `permissions:` in every workflow | 2026-11-19 |
| Release signing keys scoped away from every non-release job | `TAURI_SIGNING_*` live on the `production-release` environment; `portable`'s `publish`, `engine-update`'s `publish`, and `publish-update-feeds`'s `engine` job target `environment: production-release` | 2026-11-19 |
| A vanished update feed fails loudly instead of silently fail-opening | `feed-health.yml` asserts both configured feed URLs (`/releases/latest/download/latest.json`, `engine-latest/engine-manifest.json`) return 200 nightly; not a PR gate — it has no `pull_request`/`merge_group:` trigger and can never block the merge queue | 2026-11-19 |
| `/releases/latest` is only ever made live by a human | `publish-update-feeds` is `workflow_dispatch`-only (no schedule/push/merge) and `portable.yml` assembles `v*` as a draft; no automated path promotes a release — including the `portable` runs on `main` that only warm the caches (issue #134), which publish nothing and cannot reach `publish`/`smoke`/`promote`/`discard` (all tag-ref-only) | 2026-11-21 |
| A release tag is only signed if its commit is an ancestor of `main` | `publish` job, `Verify the tag is an ancestor of main` step | 2026-11-15 |
| Secret scanning (incl. push protection) + dependabot security updates + automated security fixes | repo → Settings → Code security and analysis | 2026-11-15 |
| Dependabot PRs auto-merge only when `.github/dependabot-policy.json` passes them (known publisher, allowlisted, not blocklisted, patch/minor incl. 0.x, release age within its window); a daily sweep re-evaluates every open Dependabot PR | workflow `dependabot-automerge.yml` + engine `scripts/dependabot-policy.sh`, single source of truth `.github/dependabot-policy.json` (trusted publishers / allowlist / blocklist / windows) | 2026-11-15 |
| Routine dependency updates batch into one grouped weekly PR per ecosystem; security-only updates stay individual and fast-laned | `.github/dependabot.yml` `groups.*` with `applies-to: version-updates` | 2026-11-15 |

> **On the free plan**, `secret_scanning_non_provider_patterns` and
> `secret_scanning_validity_checks` require **GitHub Advanced Security** and stay
> off. Re-evaluate if the org ever moves to a paid plan.

### Runners: hosted inventory + the self-hosted rule (review-enforced)

Unlike the table above, this section is not a GitHub setting — it is a
convention kept truthful by review. Every job in this repo runs on
GitHub-hosted images today — including the macOS ones, which land in the
hosted `GitHub Actions` runner group; the repo registers no self-hosted
runners of its own. The portable / engine matrices map targets to these
labels (`portable.yml`'s `os_for`):

| Target | Runner label | Kind |
| --- | --- | --- |
| `macos-arm64` | `macos-15` | hosted arm64 |
| `macos-x64` | `macos-15-intel` | hosted Intel |
| `win-vulkan-x64` | `windows-latest` | hosted |
| `ubuntu-vulkan-x64` | `ubuntu-22.04` | hosted |
| `ubuntu-vulkan-arm64` | `ubuntu-24.04-arm` | hosted |

`macos-14` was retired on 2026-11-02; the live hosted arm64 labels are
`macos-15` and `macos-26`. `macos-15-intel` is the hosted Intel image, not a
self-hosted runner.

If a self-hosted runner is ever added (issue #55), three rules hold:

- A job on it runs as the logged-in maintainer, so a self-hosted runner is
  **never** reachable from `pull_request` or `merge_group` events.
- Its label must not collide with a hosted image's label, because a job asking
  for that hosted label is matched by any self-hosted runner carrying it.
- A runner registered directly to the repository is usable by every workflow
  in it, so it belongs in a runner group whose access is restricted to the
  workflows that need it — the label is a convention, the group's access list
  is enforced.

## 2. Review policy (solo project — deliberate)

- `main` currently requires **zero approvals**: GitHub does not allow a PR
  author to approve their own PR, so an enforced 1-approval rule would make
  every solo merge require an admin bypass (which risks clobbering the whole
  ruleset). Self-merge through the queue is the working model; the enforced
  gates are the signed commits + required checks + squash queue.
- The human half is a **self-review ritual**: the PR template's checklist, a
  "Self-review" comment stating what changed and how it was verified.
  `CODEOWNERS` (`* @myopenpal-ai/openpal-maintainers`) exists and is a no-op
  until the trigger below fires.

### ⚠️ Contributor trigger

The moment a **second collaborator with write access** joins:

1. Ruleset `main: required checks + merge queue` → `required_approving_review_count: 1`.
2. Same ruleset → `require_code_owner_review: true` (CODEOWNERS is already in place).
3. Add the new maintainer to the `production-release` environment as a required
   reviewer, and re-evaluate cache-key scoping (section 4).

## 3. Why the riskier-but-necessary trade-offs are acceptable

- **`strict_required_status_checks_policy: false`** with the merge queue: the
  queue re-tests each PR against latest `main` anyway, which is what strict
  mode would do at higher cost.
- **Compile caches share one `base` scope across `push`/`merge_group`/tags**
  (PR branches get their own scope): keeps the merge queue's builds warm. A PR
  branch would need to be literally named `base` to collide with `main`'s
  scope — revisit when adding write-access collaborators.
- **Content-addressed model/classifier caches stay unscoped** (keyed by known
  SHA-256 pins): poisoning them corrupts test inputs, never shipped artifacts;
  scoping them would re-download ~400 MB per PR.
- **`portable` cache keys stay branch-agnostic, and `portable` now also runs on
  `main`**: tags, a `workflow_dispatch`, a `push` to `main` that touches a
  Rust-cache key input (a dependency or toolchain file), and a twice-weekly
  `schedule`. Only the last two resolve to the default branch, and both are
  triggers GitHub lets write the default branch's cache scope — `workflow_run`
  and `pull_request_target` cannot, so neither is used — which is the same
  write-access trust domain as pushing the release branch. Running on `main`
  is the point: cache scopes never fall back from one tag to another, so the
  default branch is the only scope a `v*` tag's first build can restore a warm
  Rust/cargo cache from (issue #134). That run publishes nothing — a branch
  run uploads no artifact, and `publish`/`smoke`/`promote`/`discard` are
  tag-ref-only — so a branch-agnostic key never carries release authority.

## 4. Owner-only steps (can't be done via PR)

- [ ] **Register an SSH signing key** on the GitHub account
  (Settings → SSH and GPG keys → New SSH signing key); enable `commit.gpgsign`
  locally (see CONTRIBUTING.md).
- [ ] **Org default workflow token = read** (Settings → Actions → General →
  Workflow permissions). Requires `admin:org`.
- [ ] **Org `web_commit_signoff_required` = on** (org Settings → Members →
  Require contributors to sign off). Requires `admin:org`.
- [ ] **Migrate `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  / `TAURI_SIGNING_PUBLIC_KEY` into Environment `production-release`**
  (Settings → Environments), paste the same values, then delete the repo-level
  copies. Until then the repo-level copies still satisfy `publish` (env secrets
  take precedence once present).
- [ ] **Verify secret scanning + push protection + DPA are green** and
  Dependabot is producing update PRs (a week after `.github/dependabot.yml`
  lands).
- [ ] Keep **no PAT with `repo`/`admin:org` scope** stored anywhere; use
  narrow GitHub App tokens for any future automation.
- [ ] On any account-security incident: rotate the Tauri keypair **and** update
  `crates/app/tauri.conf.json`'s baked-in pubkey in the same change
  (docs/RELEASING.md).

## 5. Risks deliberately not closed (tracked)

- **Private repos** (`backend`, `myopenpal.ai`): no branch protection/rulesets on
  the free plan — handled separately by the owner.
- **Single-owner fragility**: one org owner and one repo admin. A hardware
  passkey, no stored admin-scoped PATs, and a second break-glass owner (or a
  documented succession plan) are the mitigations.
- **Fork-PR compute**: any GitHub user can run arbitrary code in CI on the
  public repo's free minutes. Mitigate with the first-time-contributor workflow
  approval and revisit if abuse appears.
- **Interaction limits**: not applied proactively (would block legitimate
  community issues); apply `interaction-limits` transiently during a spam wave.
