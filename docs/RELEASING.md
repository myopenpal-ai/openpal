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
  `main` first, land only fixes there, and tag from it.
- **A signing key configured for git** (Settings → SSH and GPG keys → SSH
  signing keys, plus `git config gpg.format ssh` + `commit.gpgsign`): release
  tags are **signed tags** (the `v*`/`b*` tag ruleset requires them), and the
  `publish` job refuses to sign any tag whose commit isn't an ancestor of
  `main`.

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
git tag -s vX.Y.Z            # signed tag (required) from the merged main/release branch
git push origin vX.Y.Z       # tag push triggers the portable workflow
```

Nightly/canary tags (`b*`) use the same pipeline in a **slim form**: only the
canary subset of targets builds (`macos-arm64` + `ubuntu-vulkan-x64` by
default, overridable via the `NIGHTLY_CANARY_TARGETS` repo variable — see
`.github/workflows/portable.yml`), against the just-resolved llama.cpp engine
tag, reusing the per-target engine cache. They deliberately skip the draft
release and the smoke/promote gate; they exist to keep the full matrix honest
between stable cuts at a fraction of the cost (pushing a `b*` from `main` is
how a "per-merge" build is triggered cheaply).

### 4. Wait for the draft release

Watch Actions → **portable**. The tag-triggered run:

1. `prepare` resolves the llama.cpp engine tag once and computes the target
   matrix (canary subset for `b*`, full five for `v*`); a single `ui` job
   compiles the frontend once for every target.
2. The matrix `build` jobs assemble each bundle (restoring the shared UI and
   the per-target engine cache keyed on the resolved tag).
3. `publish` (stable cuts only) signs every artifact and assembles a
   **draft** release with them and `latest.json` — deliberately inert until a
   human promotes it (`/releases/latest` never resolves to a draft).
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

## Gotchas

- **`version-bump.yml` gates `main`** (required check, runs on `pull_request`
  and `merge_group`). If a PR is removed from the merge queue for failing it,
  the bump belongs in that PR — bump and re-queue, then merge.
- The `release: patch|minor|major` and `no-release` labels are created by
  `scripts/ensure-labels.sh` (owner-level, before the labeler and gate go
  live); the labeler keeps the `release:` one in sync with the title.
- The draft release is the intentional soft-launch gate from the auto-updater
  plan; nothing reaches users until the human promotion in step 5. The
  per-target smoke gate is the quality gate in front of it — a smoke failure
  deletes the draft automatically, so a release only survives a run that fully
  verified it.
- `TAURI_SIGNING_PUBLIC_KEY` must be the public half of the **same** keypair as
  `TAURI_SIGNING_PRIVATE_KEY`; a mismatch makes every smoke leg fail (and the
  draft get discarded), exactly as it should.
- `ui/package.json` is `"private": true` with no version — there is no
  frontend version to keep in sync; only the Rust workspace version, its
  `CARGO_PKG_VERSION` consumers and the plugin manifests.
- **Never run a blanket `cargo update`** (`time` is pinned in `Cargo.lock`).
  The bump script only rewrites the five workspace crates' entries; dependency
  bumps stay separate, deliberate PRs.
