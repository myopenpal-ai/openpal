# Releasing OpenPal

Every release is the same short chain: apply the semver rule to the commits
since the last tag → one reviewable bump commit updates every place the
version lives → push a `vX.Y.Z` tag → the `portable` workflow builds and signs
a **draft** release → a human promotes the draft.

OpenPal ships versioned, signed releases (not a continuously-deployed service),
on trunk-based development with short-lived release branches — the branch model
and tag flow are described in
[docs/TESTING.md](TESTING.md#branch-strategy-and-required-checks). This page is
the runbook a maintainer follows to cut one.

## The version rule (semver, from Conventional Commits)

The next version is decided by the most significant Conventional-Commits change
type (CONTRIBUTING.md) **merged since the last tag**:

| Commits since the last tag contain… | Bump | Example |
| --- | --- | --- |
| a breaking change (`feat!`, `fix!`, … `BREAKING CHANGE:` trailer) | **major** | v0.2.0 → v1.0.0 |
| any `feat:` | **minor** | v0.1.1 → v0.2.0 |
| only `fix:` (plus chores/docs/refactor/…) | **patch** | v0.1.0 → v0.1.1 |
| only chores/docs/refactor/… | no release | — |

`scripts/bump-version.mjs` applies this table and refuses to bump when nothing
is releasable. The bump has no effect until a tag is pushed — the app's
reported version (`CARGO_PKG_VERSION`) only changes when `portable.yml` runs on
`vX.Y.Z`.

## Cut a release

Everything below is the whole job; there are no unrecorded steps.

### 0. Prerequisites

- `git` on PATH, `gh` installed and authenticated.
- On the branch you tag from: `main` (or `release/vX.Y` when stabilizing that
  release line), freshly fetched, working tree **clean**.
- For a patch release with no stabilization period, tag from `main` directly;
  for a release that needs a stabilization window, cut `release/vX.Y` from
  `main` first, land only fixes there, and tag from it.

### 1. Bump (one reviewable step)

```bash
node scripts/bump-version.mjs --dry-run   # preview the next version + changelog section
node scripts/bump-version.mjs             # apply it
```

- The script computes the next version from the Conventional-Commits in
  `git log <last-tag>..HEAD`; the first release (no tags yet) needs an explicit
  `--target` — e.g. `node scripts/bump-version.mjs --target 0.1.0`.
- It edits **exactly** these, in one pass, and verifies the result:
  - `Cargo.toml` — `[workspace.package] version`, the single source all five
    crates read via `version.workspace = true`;
  - `Cargo.lock` — the five workspace crates' resolved versions;
  - the five built-in plugin manifests `plugins/*/plugin.json`;
  - `CHANGELOG.md` — turns `[Unreleased]` into a dated `[X.Y.Z]` section
    (generated from the commit subjects since the last tag) and opens a fresh
    `[Unreleased]`.
- It refuses to run on a dirty tree (a bump must be the entire release commit)
  and fails loudly if any file's shape changed unexpectedly.
- **Curate the generated changelog section** before committing: the script's
  output is a faithful draft from commit subjects, trimmed by the maintainer
  into what users should read. For the first release the section covers the
  full commit history — summarize it.

### 2. Review and merge the bump

Review the single diff (it must contain only the five touch-points above), then:

```bash
git add -A && git commit -m "chore(release): vX.Y.Z"
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
  as the next patch (`vX.Y.(Z+1)`).
- **Changelog**: newest first; each section is written at cut time by the bump
  script and curated in the bump PR. Never hand-edit a single version location
  on its own — if a `plugins/*/plugin.json` version ever drifts from the
  workspace version, the bundler test
  `builtin_plugin_manifests_carry_the_workspace_version` fails CI.

## Gotchas

- The draft release is the intentional soft-launch gate from the auto-updater
  plan; nothing reaches users until the human promotion in step 5.
- `ui/package.json` is `"private": true` with no version — there is no
  frontend version to keep in sync; only the Rust workspace version, its
  `CARGO_PKG_VERSION` consumers and the plugin manifests.
- **Never run a blanket `cargo update`** (`time` is pinned in `Cargo.lock`).
  The bump script only rewrites the five workspace crates' entries; dependency
  bumps stay separate, deliberate PRs.
