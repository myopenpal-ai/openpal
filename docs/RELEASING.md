# Releasing OpenPal

Releases are the sum of already-merged PRs. Every PR that changes user-facing
code decides its own version bump and adds its own changelog line **when it
merges**; CI (`version-bump.yml`) blocks anything releasable that would ship
without one. At cut time the maintainer's only job is to finalize the changelog
(`## [Unreleased]` → a dated `## [X.Y.Z]`), push a `vX.Y.Z` tag, and let the
`portable` workflow build and sign a **draft** release that a human promotes.

OpenPal ships versioned, signed releases (not a continuously-deployed service),
on trunk-based development with short-lived release branches — the branch model
and tag flow are described in
[docs/TESTING.md](TESTING.md#branch-strategy-and-required-checks). This page is
the runbook: for PR authors, when to bump; for maintainers, how to cut.

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
version-bump check passes without a bump:

- a refactor or codemod with **zero behavior change** (the byte-level diff is
  real, the user-facing effect is not);
- a change that will roll out to users under a later PR/release anyway;
- a revert that should not be re-released alone.

`[no release]` is a statement that the author and reviewer signed off on the
"no user-facing effect" reading — not an escape hatch for "I forgot to bump".
Code-only PRs that actually change behavior must bump.

## Cut a release

Everything below is the whole job; there are no unrecorded steps.

### 0. Prerequisites

- `git` on PATH, `gh` installed and authenticated.
- On the branch you tag from: `main` (or `release/vX.Y` when stabilizing that
  release line), freshly fetched, working tree **clean**.
- For a patch release with no stabilization period, tag from `main` directly;
  for a release that needs a stabilization window, cut `release/vX.Y` from
  `main` first, land only fixes there, and tag from it.

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

### 3. Tag

Tag the **merged** commit (not the local one), pushing the tag triggers the
`portable` workflow:

```bash
git fetch origin
git push origin vX.Y.Z            # tag created from the merged main/release branch
```

### 4. Wait for the draft release

Watch Actions → **portable**. The tag-triggered run builds and signs the
Portable bundles on all five targets and publishes a **draft** release with the
artifacts and `latest.json`. The first tag pays a cold, full pipeline (engine
fetch, all-platform native builds, signing) — expect it to be slow, and check
it end to end rather than assuming the trigger works.

### 5. Verify and promote

- Confirm the built app reports `X.Y.Z` (About screen) and that the draft
  release's `latest.json` lists the expected targets.
- Promote the draft — deliberately inert until now (`/releases/latest` never
  resolves to a draft, so no install auto-updates before this step):

```bash
gh release edit vX.Y.Z --draft=false --repo myopenpal-ai/openpal
```

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

## Gotchas

- **`version-bump.yml` gates `main`** (required check, runs on `pull_request`
  and `merge_group`). If a PR is removed from the merge queue for failing it,
  the bump belongs in that PR — bump and re-queue, then merge.
- The `release: patch|minor|major` and `no-release` labels are created by
  `scripts/ensure-labels.sh` (owner-level, before the labeler and gate go
  live); the labeler keeps the `release:` one in sync with the title.
- The draft release is the intentional soft-launch gate from the auto-updater
  plan; nothing reaches users until the human promotion in step 5.
- `ui/package.json` is `"private": true` with no version — there is no
  frontend version to keep in sync; only the Rust workspace version, its
  `CARGO_PKG_VERSION` consumers and the plugin manifests.
- **Never run a blanket `cargo update`** (`time` is pinned in `Cargo.lock`).
  The bump script only rewrites the five workspace crates' entries; dependency
  bumps stay separate, deliberate PRs.
