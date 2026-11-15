# Releasing OpenPal

OpenPal ships on two tracks, mirroring ggml-org/llama.cpp's release
philosophy:

- **Rolling nightly** — every *user-facing* merge to `main` auto-creates a
  monotonic `b{N}` build-number tag (`N = git rev-list --count HEAD`), which
  builds a slim **canary** subset of targets and publishes a **pre-release**.
  This is machinery, not releases: nightlies are smoke-check targets for the
  full matrix between stable cuts, never something users auto-update to.
- **Stable** — a deliberately cut `vX.Y.Z`, created only by a human running
  the manual **Make Release** workflow, which builds the full five-target
  matrix, assembles a signed **draft** release and a per-target **smoke** gate,
  and stays inert until a human promotes the draft.

The version rule and PR-driven bumping are unchanged (below); what changed is
who creates the tags: `b*` tags are pushed by CI automatically, `v*` tags are
pushed by the Make Release workflow instead of by hand from a laptop.

## The two tracks in one picture

```
user-facing PR merged to main ──► rolling-nightly ──► b{N} tag ──► portable (canary 2-target)
                                                                         └─► pre-release (X.Y.Z+b{N})
                                                                            invisible to auto-update

maintainer: bump (PRs) → finalize (PR) → Make Release ──► vX.Y.Z tag ──► portable (full 5-target)
  (dry-run first)                                                       ├─► signed draft release
                                                                        ├─► per-target smoke gate
                                                                        └─► human promote (draft=false)
```

## The version rule (semver, decided by the PR author)

The next version is the current one bumped by the most significant change the
PR ships (CONTRIBUTING.md has the Conventional-Commits conventions):

| The PR ships… | Bump | Example |
| --- | --- | --- |
| a breaking change (`feat!`, `fix!`, … `BREAKING CHANGE:` trailer) | **major** | v0.2.0 → v1.0.0 |
| any `feat:` | **minor** | v0.1.1 → v0.2.0 |
| only `fix:` (plus chores/docs/refactor/…) | **patch** | v0.1.0 → v0.1.1 |
| no user-facing change, or `[no release]` | no bump | — |

The PR author applies this table — it is *their* decision, not a computation
from git history at cut time. Two mechanisms keep it honest:

- `.github/workflows/labeler.yml` mirrors the PR title's Conventional-Commits
  type as a `release: patch|minor|major` label, so the declared bucket is
  visible on the PR.
- `version-bump.yml` is a **required check**: it compares the workspace version
  on the PR head with the merge-base and fails (naming the bump command) when
  user-facing code changed with no increase, or when the increase contradicts
  the declared `release:` label.

The bump has no effect until a tag is pushed — the app's reported version
(`CARGO_PKG_VERSION`) only changes when `portable.yml` runs on `vX.Y.Z`.

## For PR authors: bump in the same PR

When your PR changes user-facing code (anything outside `docs/`, `*.md`,
`.github/` and `scripts/`), bump the version and add your changelog line in the
**same PR**:

```bash
node scripts/bump-version.mjs --bump <patch|minor|major> --notes "One curated line for [Unreleased]"
```

- `--bump` applies the table above to the current workspace version; the script
  rewrites **exactly** these in one pass and verifies the result:
  - `Cargo.toml` — `[workspace.package] version`, the single source all five
    crates read via `version.workspace = true`;
  - `Cargo.lock` — the five workspace crates' resolved versions;
  - the five built-in plugin manifests `plugins/*/plugin.json`;
  - `CHANGELOG.md` — appends your `--notes` line(s) under `## [Unreleased]`.
- `--notes` is a curated line (or a ready-formatted block: `### Added` /
  `### Fixed` headings, `- ` bullets); a bare phrase becomes a bullet. Each PR
  adds its own line, so `[Unreleased]` grows by consensus, and no maintainer
  ever trims generated noise at cut time.
- `--target X.Y.Z` pins the version exactly instead of computing it
  (rare: backports, or a deliberate skip ahead).
- It refuses to run on a dirty tree (the bump belongs in the PR's own commits)
  and fails loudly if any file's shape changed unexpectedly. `--dry-run`
  previews the plan, `--force` overrides the dirty-tree guard; **do not** pass
  `--force` and hand-edit afterwards — the whole point is one clean, scripted,
  verified diff.

### The `[no release]` convention

Not every user-facing diff must ship a release. When yours is one of these,
add `[no release]` to the PR title (or set the `no-release` label) so the
version-bump check passes without a bump *and* the rolling-nightly workflow
skips the merge:

- a refactor or codemod with **zero behavior change** (the byte-level diff is
  real, the user-facing effect is not);
- a change that will roll out to users under a later PR/release anyway;
- a revert that should not be re-released alone.

Merges that only touch release-exempt paths (`docs/`, `*.md`, `.github/`,
`scripts/`) are skipped by `rolling.yml`'s `paths-ignore` automatically, so
they need no marker.

`[no release]` is a statement that the author and reviewer signed off on the
"no user-facing effect" reading — not an escape hatch for "I forgot to bump".
Code-only PRs that actually change behavior must bump.

## The rolling nightly track

`.github/workflows/rolling.yml` fires on every push to `main` that touches a
non-exempt path (same set as the version-bump gate). It:

1. skips cheaply (before any checkout) when the squash-merge message contains
   `[no release]`,
2. counts `N = git rev-list --count HEAD` — monotonic because `main` is
   squash-only and linear,
3. creates a lightweight `b{N}` tag and pushes it **over HTTPS with the
   `RELEASE_PUSH_TOKEN` fine-grained PAT**, so the push cascades into
   `portable.yml` (a tag push made by `GITHUB_TOKEN` would not spawn further
   workflows — see the prerequisite below),
4. a tag-exists guard plus workflow concurrency make batched queue merges safe:
   a rerun or a race that resolves the same count skips (never fails), and a
   `workflow_dispatch` on the workflow forces a nightly for current `main`.

`portable.yml`'s `prepare` job then builds the slim **canary** subset — by
default `ubuntu-vulkan-x64` + `win-vulkan-x64`, the targets with a runner in
this org today, overridable via the `NIGHTLY_CANARY_TARGETS` repo variable —
against the just-resolved llama.cpp engine tag, reusing the per-target engine
cache, and `publish` runs the bundler with `--kind nightly`: the release is a
**pre-release** whose manifest version is the workspace semver plus `+b{N}`
build metadata (`0.1.0+b45`).

**What nightlies are and are not.** They are canary targets only: a cheap,
per-merge signal that the full matrix is healthy between stable cuts, at a
fraction of the cost. They are *not* updater feeds: `/releases/latest` never
resolves to a pre-release, so no installed app ever auto-updates to a nightly
(`crates/core/src/update.rs` is untouched by this track). They are *not*
promised artifacts: a nightly tag can be deleted/replaced without ceremony.

## Cut a stable release

Everything below is the whole job; there are no unrecorded steps.

### 0. Prerequisites

- `git` on PATH, `gh` installed and authenticated.
- The **`RELEASE_PUSH_TOKEN` fine-grained Personal Access Token** — an
  operator step, not code. Because tags pushed by `GITHUB_TOKEN` cannot spawn
  further workflow runs, both tag-pushing workflows (`rolling.yml` and
  `make-release.yml`) push the tag over HTTPS authenticated with this token:
  - create it at GitHub → Settings → Developer settings → Personal access
    tokens → **Fine-grained tokens**, owner `myopenpal-ai`, repository access
    **Only select repositories** → `openpal`, repository permission
    **Contents: Read and write**, and an expiry (rotate before it lapses);
  - store it as the `RELEASE_PUSH_TOKEN` **Actions secret** on this repo
    (Settings → Secrets and variables → Actions; it is shown only once at
    creation, so set it immediately).
  Without it, the `b*`/`v*` tag cascade into `portable.yml` never fires and
  the workflows fail at the push step.
- The updater signing keypair's Actions secrets configured on the repo (they
  gate the release pipeline — see below):
  - `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the
    Tauri updater key the `publish` job signs artifacts with;
  - `TAURI_SIGNING_PUBLIC_KEY` — the minisign public key half of that **same**
    keypair, required by the per-target smoke gate (`smoke` fails closed
    without it). It is the bare `RW…` string: if your keypair is stored as a
    `.pub` file, base64-decode the file and take its second line.
- The same `RW…` string is **baked into the app**: `crates/app/tauri.conf.json`
  → `plugins.updater.pubkey`. That compiled-in key is what an installed app
  verifies downloaded artifacts against — the secret above only gates CI. When
  the keypair rotates, update the three secrets *and* this file in the same
  change, or the smoke gate passes while real installs reject every artifact.
- For a patch release with no stabilization period, tag from `main` directly;
  for a release that needs a stabilization window, cut `release/vX.Y` from
  `main` first, land only fixes there, and tag from it. **Make Release
  validates against `main`** — a release-branch cut passes `skip_checks`
  deliberately, or is cut from `main` after the fixes merge back.

### 1. Finalize (one reviewable step)

By cut time the version is already correct — the last user-facing PR bumped it
— and `[Unreleased]` already holds every releasable change's curated line:

```bash
node scripts/bump-version.mjs finalize --dry-run   # preview
node scripts/bump-version.mjs finalize             # apply it
```

`finalize` renames `## [Unreleased]` into a dated `## [X.Y.Z]` section (using
the current workspace version) and opens a fresh, empty `[Unreleased]` for the
next cycle. It changes `CHANGELOG.md` only — the version touch-points were
already bumped by the PRs. Same guards as the bump: it refuses a dirty tree,
verifies its work, and honours `--dry-run` and `--force`.

Sanity-check the section before committing: it should read like a release note
for humans. If a merged PR's line needs polishing, this is the PR in which to
do it (amend the changelog here, not the version files).

### 2. Review and merge the finalize

Review the single diff (it must contain `CHANGELOG.md` only), then:

```bash
git add -A && git commit -m "chore(release): finalize CHANGELOG for vX.Y.Z"
# push the branch, open a PR (body: Closes #<the release ticket>), get the
# required checks green, squash-merge.
```

> The change is release-exempt (`CHANGELOG.md` is a `*.md`), so no bump is
> needed and this PR intentionally does **not** carry `[no release]`-style
> markers governing the next version.

### 3. Run the Make Release workflow

Tags are pushed by the workflow, not by hand — **Actions → Make Release →
Run workflow**:

1. **Dry run first** (the default `dry_run: true`): pick the commit (leave
   empty for `main` HEAD) and run. It validates that the chosen commit belongs
   to `main`, is not stale vs HEAD, and that the workspace version on it is
   strictly newer than the last `v*` tag — otherwise it fails naming the bump
   command (`node scripts/bump-version.mjs --bump …`), because version bumps
   are PR-driven and must land before the cut.
2. **Re-run with `dry_run: false`**: the workflow creates an **annotated
   `vX.Y.Z` tag** and pushes it with `RELEASE_PUSH_TOKEN`, which cascades into
   `portable.yml`. Its summary then carries the promote instructions (gated on
   the per-target smoke check).

`skip_checks` exists for deliberate exceptions (cutting an older commit, a
release-line tag); use it only when you know why — the checks catch real
mistakes.

### 4. Wait for the draft release

Watch Actions → **portable**. The tag-triggered run:

1. `prepare` resolves the llama.cpp engine tag once and computes the target
   matrix (canary subset for `b*`, full five for `v*`); a single `ui` job
   compiles the frontend once for every target.
2. The matrix `build` jobs assemble each bundle (restoring the shared UI and
   the per-target engine cache keyed on the resolved tag).
3. `publish` (this track is `v*`, so `--kind stable`) signs every artifact and
   assembles a **draft** release with them and `latest.json` — deliberately
   inert until a human promotes it (`/releases/latest` never resolves to a
   draft).
4. The per-target `smoke` matrix downloads each shipped bundle from the draft
   and verifies it end to end: the cut's version in `latest.json`, the
   artifact's SHA-256 and updater signature, the version the bundle itself
   reports, and that the packaged engine executes.

The first tag for an engine bump pays a cold, full pipeline (engine fetch on
all-platform native builds, signing) — expect it to be slow, and check it end
to end rather than assuming the trigger works.

### 5. Verify and promote

- If every smoke leg passed, the final job's summary says **✓ Smoke passed —
  promote** and prints the one command. Promote the draft — the only step
  that makes the release live (until now `/releases/latest` deliberately
  doesn't resolve to it, so no install auto-updates):

```bash
gh release edit vX.Y.Z --draft=false --repo myopenpal-ai/openpal
```

- If any smoke leg failed, the run instead deletes the draft and its summary
  says **❌ Smoke failed — draft discarded**; **no promote instructions are
  issued**. Investigate the failing leg, fix the build, and re-cut.

Done: users' in-app update check now finds `vX.Y.Z`.

## After a release

- **Hotfixes**: branch from the tag/release branch, PR back into that release
  branch and cherry-pick forward to `main` (docs/TESTING.md) — a `fix` lands
  as the next patch (`vX.Y.(Z+1)`), bumped in that hotfix PR like any other.
- **Changelog**: newest first; each section is assembled one PR at a time
  under `[Unreleased]` and dated at cut time by `finalize`. Never hand-edit a
  single version location on its own — if a `plugins/*/plugin.json` version
  ever drifts from the workspace version, the bundler test
  `builtin_plugin_manifests_carry_the_workspace_version` fails CI.
- **Nightlies** need no after-action: the next user-facing merge tops up the
  `b{N}` sequence by itself.

## Gotchas

- **`version-bump.yml` gates `main`** (required check, runs on `pull_request`
  and `merge_group`). If a PR is removed from the merge queue for failing it,
  the bump belongs in that PR — bump and re-queue, then merge.
- The `release: patch|minor|major` and `no-release` labels are created by
  `scripts/ensure-labels.sh` (owner-level, before the labeler and gate go
  live); the labeler keeps the `release:` one in sync with the title.
- **`RELEASE_PUSH_TOKEN` is an operator step.** Both tag-pushing workflows
  fail at the push if the secret is missing — the failure message says so.
  This is intentional: the token is a credential whose pushes trigger CI,
  which `GITHUB_TOKEN` never can. Scope it to this one repository with an
  expiry, and rotate before it lapses or if it ever leaks (a leaked token
  should be revoked and regenerated, then the secret updated).
- The draft release is the intentional soft-launch gate from the auto-updater
  plan; nothing reaches users until the human promotion in step 5. The
  per-target smoke gate is the quality gate in front of it — a smoke failure
  deletes the draft automatically, so a release only survives a run that fully
  verified it. Nightlies skip the smoke gate by design (canary subset, and
  nothing auto-updates to them).
- `TAURI_SIGNING_PUBLIC_KEY` must be the public half of the **same** keypair as
  `TAURI_SIGNING_PRIVATE_KEY`; a mismatch makes every smoke leg fail (and the
  draft get discarded), exactly as it should.
- `ui/package.json` is `"private": true` with no version — there is no
  frontend version to keep in sync; only the Rust workspace version, its
  `CARGO_PKG_VERSION` consumers and the plugin manifests.
- **Never run a blanket `cargo update`** (`time` is pinned in `Cargo.lock`).
  The bump script only rewrites the five workspace crates' entries; dependency
  bumps stay separate, deliberate PRs.
