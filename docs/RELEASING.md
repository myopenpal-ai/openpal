# Releasing OpenPal

OpenPal ships on two tracks, mirroring ggml-org/llama.cpp's release
philosophy:

- **Rolling nightly** — every *user-facing* merge to `main` auto-creates a
  monotonic `b{N}` build-number tag (`N = git rev-list --count HEAD`), which
  builds a slim **canary** subset of targets and publishes a **pre-release**.
  This is machinery, not releases: nightlies are smoke-check targets for the
  full matrix between stable cuts, never something users auto-update to.
- **Stable** — a deliberately cut `vX.Y.Z`, created only by a human running
  the manual **Make Release** workflow, which builds the stable matrix (all
  five targets, Apple Silicon included; all on GitHub-hosted runners),
  assembles a signed **draft** release and a per-target **smoke** gate, and
  stays inert until a human promotes the draft.

The version rule and PR-driven bumping are unchanged (below); what changed is
who creates the tags: `b*` tags are pushed by CI automatically, `v*` tags are
pushed by the Make Release workflow instead of by hand from a laptop.

## The two tracks in one picture

```
user-facing PR merged to main ──► rolling-nightly ──► b{N} tag ──► portable (canary 2-target)
                                                                         └─► pre-release (X.Y.Z+b{N})
                                                                            invisible to auto-update

maintainer: bump (PRs) → finalize (PR) → Make Release ──► vX.Y.Z tag ──► portable (stable matrix)
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
- `version-bump.yml` is a **required check** (runs on `pull_request` and
  `merge_group`): it compares the workspace version on the PR head with the
  merge-base and fails when user-facing code changed with no increase, or when
  the increase contradicts a declared `release:` label. When the only problem
  is a forgotten bump on an in-repo branch, CI applies the bump itself as a
  commit on the PR head (["If you forgot to bump"](#if-you-forgot-to-bump-ci-does-it-for-you-in-repo-prs)
  below); fork PRs still get the failure naming the exact bump command.

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
  - the built-in plugin manifests `plugins/*/plugin.json` (enumerated
    automatically, so a new plugin can never be forgotten);
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

### If you forgot to bump: CI does it for you (in-repo PRs)

The gate distinguishes "no bump at all" from an author decision (a wrong bucket
or conflicting `release:` labels get guidance, never a silent override). When a
PR changes user-facing code, carries no `[no release]` opt-out, and the
workspace version did **not** increase since the merge-base, `version-bump.yml`
bumps it itself: one commit pushed to the PR head —
`chore(release): auto-bump workspace version to vX.Y.Z` — performing the exact
rewrite `node scripts/bump-version.mjs --bump <bucket> --notes "<PR title>"`
would (same `Cargo.toml` / `Cargo.lock` / `plugins/*/plugin.json` /
`CHANGELOG.md` touch-points, same verification). The workflow then waits for
the PR's re-triggered check to appear on the new head and **fails red** if it
never starts — a `GITHUB_TOKEN` push moves the branch and starts nothing, so
the push cannot be trusted on its own (issue #131). The check passes on the
re-run. The bucket is the declared `release: patch|minor|major` label when one
is present, otherwise the same suggestion the check prints (`feat` → minor,
anything else → patch); the `## [Unreleased]` line is the PR title with its
Conventional-Commits `type(scope):` prefix stripped. The scheme is idempotent:
once the bump commit is in the PR's history the check passes, and the
merge-queue run needs no further action.

This is a convenience, not a licence to ignore the table — review the bot's
commit like any other, and the automation only fires for the "forgot to bump"
case above. It never pushes to a fork's head branch: a **fork PR** keeps the
old behaviour, the check fails naming the exact bump command and the author
runs it by hand.

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
3. signs a `b{N}` tag in the runner (`git tag -s` with `GIT_SIGNING_KEY` — the
   tag ruleset requires signed `b*` tags) and pushes it **over HTTPS with the
   `RELEASE_PUSH_TOKEN` fine-grained PAT**, so the push cascades into
   `portable.yml` (a tag push made by `GITHUB_TOKEN` would not spawn further
   workflows — see the prerequisites below),
4. a tag-exists guard plus workflow concurrency make batched queue merges safe:
   a rerun or a race that resolves the same count skips (never fails), and a
   `workflow_dispatch` on the workflow forces a nightly for current `main`.

`portable.yml`'s `prepare` job then builds the slim **canary** subset — by
default `ubuntu-vulkan-x64` + `win-vulkan-x64`, the two cheapest legs,
overridable via the `NIGHTLY_CANARY_TARGETS` repo variable —
against the just-resolved llama.cpp engine tag, reusing the per-target engine
cache, and `publish` runs the bundler with `--kind nightly`: the release is a
**pre-release** whose manifest version is the workspace semver plus `+b{N}`
build metadata (`0.1.0+b45`).

**What nightlies are and are not.** They are canary targets only: a cheap,
per-merge signal that the full matrix is healthy between stable cuts, at a
fraction of the cost. They are *not* updater feeds: `/releases/latest` never
resolves to a pre-release, so no installed app ever auto-updates to a nightly
(`crates/core/src/update.rs` is untouched by this track). They are *not*
promised artifacts — but their tags are not disposable either: the release-tag
ruleset makes every `b*` tag signed, immovable and undeletable, so a nightly
that misses its build is shipped or retired like any other tag, never deleted
or re-pointed ([Release tags are immovable](#release-tags-are-immovable)).

## Release tags are immovable

The repository ruleset **`release tags: signed, immovable, undeletable`**
applies to `refs/tags/v*` and `refs/tags/b*` with `bypass_actors: []` and
enforces exactly three rules:

| Rule | Effect |
| --- | --- |
| `required_signatures` | every `v*`/`b*` tag must be signed; the workflows sign in the runner with `GIT_SIGNING_KEY` |
| `non_fast_forward` | a tag cannot be moved to another commit — it cannot be re-pointed or made to fire again |
| `deletion` | a tag cannot be deleted — not by `git push --delete`, not by the API |

There is no bypass actor, so nothing is exempt by default — not the workflows,
not repository admins. The only way to delete one is an operator-level ruleset
change (below). Consequences worth knowing before a cut:

- **A tag is permanent.** "Delete it and re-tag" is not a recovery path; a
  re-run of Make Release for an existing version is refused by design.
- **Every tag must end with a release.** A `v*`/`b*` tag with no release is an
  orphan: public, undeletable, and (for `v*`) invisible to
  `/releases/latest`. The rule is **each tag has a release, or it is
  retired**: ship it (a tag-ref dispatch of `portable.yml`, see
  [recovery](#if-a-tag-exists-but-its-build-never-started-recovery)), or retire
  it with an operator-level ruleset change (below). Never leave one without a
  decision.
- **A tag stays retired.** Retiring is for a tag that will never be shipped
  (for example a `v*` cut whose draft the smoke gate discarded — fix it under
  the *next* version, which carries the fix). Never retire and then re-cut the
  same version.
- **Retiring a tag is an operator action.** No workflow can do it. Add a
  bypass actor to the ruleset (Settings → Rules → the `release tags` ruleset →
  Bypass list — a repository admin or app), delete the tag and its release
  (`gh api -X DELETE repos/{owner}/{repo}/git/refs/tags/<tag>` and
  `gh release delete <tag> --yes`), then remove the bypass actor again. Record
  the tag and the reason in the release ticket, and remove the bypass as soon
  as the deletion is done.

## Cut a stable release

Everything below is the whole job; there are no unrecorded steps.

### 0. Prerequisites

- `git` on PATH, `gh` installed and authenticated.
- The **`RELEASE_PUSH_TOKEN` fine-grained Personal Access Token** — an
  operator step, not code. Because pushes made by `GITHUB_TOKEN` never spawn
  further workflow runs, every workflow that pushes over HTTPS authenticates
  with this token: the tag-pushing workflows (`rolling.yml` and
  `make-release.yml`) push `b*`/`v*` tags, and `version-bump.yml` pushes its
  automated PR-head bump commit (a `GITHUB_TOKEN` push could not re-trigger the
  PR's checks, so the gate would never re-run green):
  - create it at GitHub → Settings → Developer settings → Personal access
    tokens → **Fine-grained tokens**, owner `myopenpal-ai`, repository access
    **Only select repositories** → `openpal`, repository permission
    **Contents: Read and write**, and an expiry (rotate before it lapses);
  - store it as the `RELEASE_PUSH_TOKEN` **Actions secret** on this repo
    (Settings → Secrets and variables → Actions; it is shown only once at
    creation, so set it immediately).
  Without it, the push steps fail immediately naming the missing secret, and
  the auto-bump never lands — the version-bump gate then fails red naming the
  bump command, exactly as it did before the automation.
- **Every push proves it had an effect.** The three pushing workflows do not
  trust the push's exit code: `rolling.yml` and `make-release.yml` wait for the
  `portable` run on exactly the new tag and commit, and `version-bump.yml`
  waits for the PR's re-triggered check on the new head. All three fail **red**
  naming the run that should exist when nothing starts. This matters because
  the failure mode is invisible otherwise: a push authenticated with
  `GITHUB_TOKEN` succeeds, moves the tag or branch, and GitHub starts no
  workflow for it — the run that looks green is the one that did nothing
  (issue #131). `actions/checkout` runs with `persist-credentials: false` in
  those workflows so the only credential is the `RELEASE_PUSH_TOKEN` remote;
  the push steps also refuse to run if a GitHub Authorization header is
  visible to git at all.
- The **`GIT_SIGNING_KEY` SSH private key** — the `v*`/`b*` tag ruleset
  requires **signed tags**, so both tag-pushing workflows sign the tag in the
  runner with this key:
  - generate a dedicated ed25519 keypair (never reuse a personal key):
    `ssh-keygen -t ed25519 -N "" -C "openpal-actions-release-tag-signing" -f id_ed25519`;
  - register the **public** half as an **SSH signing key** on the operator's
    GitHub account (Settings → SSH and GPG keys → New SSH key → key type
    *Signing Key*) — GitHub resolves the signature to that account;
  - store the **private** half's content exactly as the `GIT_SIGNING_KEY`
    **Actions secret** on this repo (Settings → Secrets and variables →
    Actions; shown only once at creation);
  - rotate it (new keypair, re-register the public half, update the secret)
    on a schedule or if it ever leaks.
  Without it, tag pushes are rejected by the `v*`/`b*` ruleset's
  `required_signatures` — the workflows fail at the signing step with a
  message naming the missing secret.
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
- **Release tags are signed** — the `v*`/`b*` tag ruleset requires
  `required_signatures` (and makes tags immovable and undeletable). Tags are
  created by the workflows, which sign them in the runner with the
  `GIT_SIGNING_KEY` from the prerequisite above; the `publish` job
  additionally refuses to sign any tag whose commit isn't an ancestor of
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

> The change is release-exempt (`CHANGELOG.md` is a `*.md`), so no bump is
> needed and this PR intentionally does **not** carry `[no release]`-style
> markers governing the next version.

### 3. Run the Make Release workflow

Tags are created and pushed by the workflow, **not by hand** — **Actions →
Make Release → Run workflow**. The workflow signs the tag in the runner with
`GIT_SIGNING_KEY` (the ruleset requires signed `v*`/`b*` tags), so no personal
git signing setup is needed:

1. **Dry run first** (the default `dry_run: true`): pick the commit (leave
   empty for `main` HEAD) and run. It validates that the chosen commit belongs
   to `main`, is not stale vs HEAD, and that the workspace version on it is
   strictly newer than the last `v*` tag — otherwise it fails naming the bump
   command (`node scripts/bump-version.mjs --bump …`), because version bumps
   are PR-driven and must land before the cut. Two carve-outs keep a dry run
   honest without ever punishing a correct state:
   - a dry run on a version whose `vX.Y.Z` tag **already exists** (an earlier
     real cut already shipped it) exits **green as an "already cut" no-op** —
     that version is released, nothing to validate, nothing to create;
   - the `vX.Y.Z` tag merely existing in the repo therefore never makes a dry
     run red; the "not strictly newer" failure is reserved for genuine
     "you never bumped" states (the version's tag does not exist yet).
2. **Re-run with `dry_run: false`**: the workflow creates a **signed annotated
   `vX.Y.Z` tag** (`git tag -s` with `GIT_SIGNING_KEY`) and pushes it with
   `RELEASE_PUSH_TOKEN`, which cascades into `portable.yml`. It then **waits
   for that `portable` run to start** — on exactly the new tag and commit —
   and only goes green once it has; if none appears within 120 s the run fails
   **red**, naming the run that should exist and the recovery command. Its
   summary then links the run and carries the promote instructions (gated on
   the per-target smoke check).

Every run is self-labelled in the Actions run list as **`Make Release — dry
run`** or **`Make Release — real cut`**, and overlapping dispatches (say a dry
run and a real cut fired seconds apart on the same commit) are **queued by a
`concurrency` guard** instead of raced. So the order above is safe in either
direction: whichever run lands first, no run is left red by the sibling run —
a real cut followed by a dry run on the same version simply gets the green
"already cut" no-op from the bullet above.

`skip_checks` exists for deliberate exceptions (cutting an older commit, a
release-line tag); use it only when you know why — the checks catch real
mistakes.

### 4. Wait for the draft release

Watch Actions → **portable**. The Make Release run (or `rolling-nightly` for a
nightly) has already waited for this run to start and went green on that, so
the tag-pushing run being green is the proof the cascade happened — a tag that
missed its build fails there, red, naming the run. The tag-triggered run:

1. `prepare` resolves the llama.cpp engine tag once and computes the target
   matrix (canary subset for `b*`, all five targets for `v*`) — every leg runs
   on a GitHub-hosted image, so `macos-arm64` is in by default; a
   `STABLE_TARGETS` repo variable can trim a deliberate exception, and
   `prepare` warns if that override drops `macos-arm64`. A single `ui` job
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

   Or skip the raw command: run the **`publish-update-feeds`** workflow
   (dispatch-only, no schedule) with `promote_tag = vX.Y.Z`. It refuses
   non-`v*` tags and pre-releases, treats an already-published tag as a
   no-op, promotes the draft, then verifies both update feeds resolve — the
   promote is still a deliberate human action, just scripted.

- If any smoke leg failed, the run instead deletes the draft and its summary
  says **❌ Smoke failed — draft discarded**; **no promote instructions are
  issued**. Investigate the failing leg, fix the build, and cut the *next*
  version — the failed tag is permanent, so retire it per [Release tags are
  immovable](#release-tags-are-immovable). Never re-cut a version whose tag
  exists; Make Release refuses it.

Done: users' in-app update check now finds `vX.Y.Z`.

### If a tag exists but its build never started (recovery)

The tag-pushing runs verify the cascade (above), so this is for a tag that
predates that check or whose trigger was lost another way. **Do not re-cut** —
the tag is immovable and Make Release refuses an existing tag. Ship the tag
once `RELEASE_PUSH_TOKEN` is fixed:

```bash
gh workflow run portable.yml --ref vX.Y.Z
```

A dispatch on a **tag** runs the same pipeline as the tag push: `prepare`
builds that tag's matrix (stable for `v*`, canary for `b*`), `publish` signs
and assembles the release for that tag (`--kind stable` or `--kind nightly`),
and `smoke`/`promote`/`discard` behave exactly as they do for a push. The
optional model pre-stage input is refused on a tag ref — a release carries the
standard SKU. A branch dispatch still builds a one-off and assembles no
release. The `portable` concurrency group cancels an in-progress run on the
same ref, so dispatch only when the tag's push-triggered run is absent or
finished.

This needs the tag's commit to contain the tag-dispatch gate in
`portable.yml`: `--ref <tag>` runs the workflow file as it existed at that tag.
Tags cut before the gate can only build their artifacts on a dispatch, not
their release, so those are retired with an operator bypass ([Release tags are
immovable](#release-tags-are-immovable)) or shipped by cutting the next
version.

### Full SKU (installed app) cuts

The Portable pipeline above builds the no-install bundles in CI. The **Full
SKU** — the signed/notarized `.app` + `.dmg` a user installs — is built and
published from a machine (a Mac, per `crates/app/tauri.conf.json`'s
`["app", "dmg"]` targets), per platform (a macOS Apple Silicon box produces
the `darwin-aarch64` entry, an Intel one `darwin-x86_64`):

1. Build, sign and notarize locally:

```bash
cargo run -p openpal-bundler -- release           # build + sign + notarize
```

   (`bundler release` runs `build` → `sign` → `notarize`; set `SIGNING_IDENTITY`
   and the Apple notarization credentials to sign/notarize, or run those three
   commands separately.) The bundle output lands under
   `target/release/bundle/` (`macos/` + `dmg/`). The manifest command never
   depends on Tauri's `createUpdaterArtifacts` flag: it archives the *final*
   signed/notarized `.app` itself, so the shipped artifact is exactly the
   verified product.

2. Once the tag's draft release exists (the portable pipeline assembled it),
   add this machine's Full-SKU leg to the same manifest and upload its
   artifacts — the command signs the archive, derives each platform's
   download URL from the actual file name, and uploads with `--clobber`, so
   the manifest's `platforms` URLs always match what the release carries (no
   hand-editing; this is what keeps in-app install from 404ing). Needs the
   updater `TAURI_SIGNING_PRIVATE_KEY`/`_PASSWORD` env vars, like any
   publish run:

```bash
cargo run -p openpal-bundler -- manifest \
  --tag vX.Y.Z --full-dir target/release/bundle
```

   Run this on every machine a Full SKU was built for (arm64 + Intel
   together produce both macOS entries); each run merges into the draft's
   `latest.json` rather than overwriting it.

3. Verify end to end on each Full-SKU machine before promoting: install the
   cut, wait for the 5-second auto-check (or Settings → "Check for updates
   now"), and confirm the **download and install** step — not just the check —
   completes and the app relaunches on the new version, with no `404` /
   `update install failed` in the log.

4. Promote the draft with the same command as the Portable cut (step 5
   above) — one release carries both SKUs' artifacts and manifest blocks.

Nightly (`b*`) cuts never go through this path: `--full-dir` is only ever
run by a human against a `v*` cut, and pre-releases are invisible to the
auto-updater anyway.

### Engine updates feed (`engine-latest`)

The in-app engine updater (`crates/core/src/engine_update.rs`) reads its own
rolling channel, independent of full OpenPal releases: a fixed `engine-latest`
GitHub Release carrying `engine-manifest.json` plus one signed flat zip per
platform target (`macos-arm64`, `macos-x64`, `win-vulkan-x64`,
`ubuntu-vulkan-x64`, `ubuntu-vulkan-arm64` — the bundler `--target` strings,
same vocabulary as `fetch-engine`).

**It is published by CI, nightly** — the `engine-update.yml` workflow loops
the target set (one sequential job, because `engine-manifest` fetch-and-
merges the whole manifest file: concurrent legs would race and drop each
other's entries), running `fetch-engine` + `engine-manifest` per target from
a single ubuntu-latest runner. The first successful run also creates the
`engine-latest` release itself (`engine-manifest` publishes **non-draft**,
unlike a `v*` cut's draft/promote gate), so **no manual bootstrap is
needed** once the signing secrets live on the `production-release`
environment (docs/SECURITY_POSTURE.md). The default target set is the full
five-target inventory, the same set `portable.yml`'s stable matrix builds —
`macos-arm64` included, because the loop stages and packages engine archives
on `ubuntu-latest` and `fetch-engine` only smoke-tests a binary the host can
execute. The set is overridable with the `ENGINE_TARGETS` repo variable, and
`prepare` warns if that override drops `macos-arm64`. The `feed-health.yml`
workflow checks both configured feeds return 200 nightly: the only alarm a
vanished feed has, because the update gate fails open.

The bundler commands remain the manual path (e.g. publishing a target
outside the scheduled set) — or run the **`publish-update-feeds`** workflow
(dispatch-only) with `engine_refresh` and optional `engine_targets` /
`engine_tag` inputs for the guided version, which verifies the feed
afterward. Each run re-stages the engine dir for that target, then packages,
signs, and merges its manifest entry while leaving the other targets'
entries untouched:

```bash
cargo run -p openpal-bundler -- fetch-engine --target <target>
cargo run -p openpal-bundler -- engine-manifest --target <target>
```

The first `engine-manifest` run creates the **non-draft** `engine-latest`
release; later runs `--clobber`-replace that target's asset alongside the
merged manifest. Needs `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` and an authenticated `gh`. Any OS can
package any target — `fetch-engine` downloads the upstream llama.cpp asset
directly and skips its host smoke test for foreign targets.

Until `engine-latest` exists, the configured engine feed 404s and the engine
update check/gate fail open on every install — verify it resolves before
relying on in-app engine updates:

```bash
curl -fsSL https://github.com/myopenpal-ai/openpal/releases/download/engine-latest/engine-manifest.json
```

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
  and `merge_group`). For in-repo PRs the gate fixes the one "forgot to bump"
  failure itself — a commit on the PR head — so a PR removed from the merge
  queue for failing it needs a human only when the automation could not or
  should not act: a fork PR, conflicting `release:` labels, a declared bucket
  contradicted by an actual bump, or a bump that still has not landed. Fix the
  cause, then re-queue.
- The `release: patch|minor|major` and `no-release` labels are created by
  `scripts/ensure-labels.sh` (owner-level, before the labeler and gate go
  live); the labeler keeps the `release:` one in sync with the title.
- **`RELEASE_PUSH_TOKEN` and `GIT_SIGNING_KEY` are operator steps.** The three
  pushing workflows fail with a message naming the missing secret — at the push
  step for the token, at the signing step for the key — and, if a push somehow
  does not start its downstream run, the verification step after it fails red
  naming the run that should exist. This is intentional: the token is a
  credential whose pushes trigger CI (which `GITHUB_TOKEN` never can — scope it
  to this one repository and rotate before it lapses), and the key is what lets
  CI-created tags satisfy the signed-tag ruleset (register its public half on
  your GitHub account; rotate it on a schedule or if it ever leaks).
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
