# Security posture (repo, CI, org)

This page is the single source of truth for OpenPal's repo/CI security
invariants. PRs and settings changes that break one of these are a regression
even if CI is green. It complements [SECURITY.md](../SECURITY.md) (vulnerability
reporting and product threat model) and [RELEASING.md](RELEASING.md) (release
runbook).

Last reviewed: 2026-11-15 (posture hardening, P0–P1).

---

## 1. Machine-enforced invariants

These are enforced by GitHub settings; none of them can be silently dropped by a
normal PR.

| Invariant | Where enforced | Since |
| --- | --- | --- |
| `main` is PR-only, squash-only, no force-push, no deletion | ruleset `main: required checks + merge queue`, **no bypass actors** | 2026-11-12 |
| 11 required status checks + merge queue on `main` | same ruleset (`required_status_checks`, `merge_queue`) | 2026-11-12 |
| Every commit merged to `main` is **signed and verified** | same ruleset, `required_signatures` rule | 2026-11-15 |
| Release/nightly tags are **signed**, can't be moved or deleted | tag ruleset `v*`/`b*`: `required_signatures`, `non_fast_forward`, `deletion` | 2026-11-15 |
| Tags pushed from CI are **signed in the runner** with a dedicated key, never a personal one | `rolling.yml` / `make-release.yml` import `GIT_SIGNING_KEY` and `git tag -s`; public half registered as an SSH signing key on the operator account | 2026-11-15 |
| All third-party actions pinned to a full 40-char commit SHA | CI gate `action-pins` in `ci.yml` (fails any floating `@tag` ref) | 2026-11-15 |
| Least-privilege `GITHUB_TOKEN`: `contents: read` default, `actions: write` only on the `frontend` job, `contents: write` + `deployments: write` only on `portable`'s `publish`/`smoke`/`discard` (`smoke` needs write to read the *draft* release it verifies — drafts 404 for read-only tokens), `engine-update`'s `publish`, and `publish-update-feeds` (`promote` needs `contents: write` to edit the release; its `engine` job needs `contents: write` + `deployments: write`) | `permissions:` in every workflow | 2026-11-19 |
| Release signing keys scoped away from every non-release job | `TAURI_SIGNING_*` live on the `production-release` environment; `portable`'s `publish`, `engine-update`'s `publish`, and `publish-update-feeds`'s `engine` job target `environment: production-release` | 2026-11-19 |
| A vanished update feed fails loudly instead of silently fail-opening | `feed-health.yml` asserts both configured feed URLs (`/releases/latest/download/latest.json`, `engine-latest/engine-manifest.json`) return 200 nightly; not a PR gate — it has no `pull_request`/`merge_group:` trigger and can never block the merge queue | 2026-11-19 |
| `/releases/latest` is only ever made live by a human | `publish-update-feeds` is `workflow_dispatch`-only (no schedule/push/merge) and `portable.yml` assembles `v*` as a draft; no automated path promotes a release | 2026-11-19 |
| A release tag is only signed if its commit is an ancestor of `main` | `publish` job, `Verify the tag is an ancestor of main` step | 2026-11-15 |
| Secret scanning (incl. push protection) + dependabot security updates + automated security fixes | repo → Settings → Code security and analysis | 2026-11-15 |
| Dependabot PRs auto-merge only when `.github/dependabot-policy.json` passes them (known publisher, allowlisted, not blocklisted, patch/minor incl. 0.x, release age within its window); a daily sweep re-evaluates every open Dependabot PR | workflow `dependabot-automerge.yml` + engine `scripts/dependabot-policy.sh`, single source of truth `.github/dependabot-policy.json` (trusted publishers / allowlist / blocklist / windows) | 2026-11-15 |
| Routine dependency updates batch into one grouped weekly PR per ecosystem; security-only updates stay individual and fast-laned | `.github/dependabot.yml` `groups.*` with `applies-to: version-updates` | 2026-11-15 |

> **On the free plan**, `secret_scanning_non_provider_patterns` and
> `secret_scanning_validity_checks` require **GitHub Advanced Security** and stay
> off. Re-evaluate if the org ever moves to a paid plan.

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
- **`portable` cache keys stay branch-agnostic**: the workflow only runs on
  tags/`workflow_dispatch`, both of which require write access (the same trust
  domain as pushing the release branch).

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
