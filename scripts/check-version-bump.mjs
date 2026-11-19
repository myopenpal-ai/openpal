#!/usr/bin/env node
// The version-bump CI gate (version-bump.yml runs this on pull_request and
// merge_group). Version bumps are PR-driven: the author of any user-facing
// change must bump the workspace version in the SAME PR
// (`node scripts/bump-version.mjs --bump patch|minor|major --notes "…"`), so
// this check only has to compare the workspace version between the PR head and
// the merge-base — no git-log archaeology, no Conventional-Commits guessing.
//
// It passes when:
//   - every changed path is release-exempt (docs/, *.md, .github/, scripts/),
//     or
//   - the PR opts out with `[no release]` in its title or the `no-release`
//     label, or
//   - the workspace version increased since the merge-base. When `release:
//     patch|minor|major` labels are declared, the actual delta must match the
//     declared bucket; unlabeled PRs pass on any increase.
//
// Exit codes:
//   0  pass.
//   1  fail — the author must act (a declared `release:` label contradicts an
//      actual bump, release labels conflict, …): prints the exact bump command.
//   3  fail *only* because the version never increased since the merge-base
//      while user-facing code changed with no `[no release]` opt-out — the one
//      case CI can fix on its own (issue #111). Still prints the human
//      guidance (for forks and the merge-queue ref, which no one can push to,
//      that is the whole story), plus an `AUTO_BUMP_BUCKET=`/`AUTO_BUMP_NOTE=`
//      payload; version-bump.yml applies that exact bump as a commit on in-repo
//      PR head branches and pushes it, so the gate re-runs green.
//
// This is a REQUIRED check on main, so a failing run blocks the merge queue.
//
//   node scripts/check-version-bump.mjs --base <sha> --head <sha> \
//       [--title "…"] [--labels "release: minor type/chore"]
//
// Needs git and a full fetch (the merge-base must be resolvable).

import { execFileSync } from "node:child_process";

const args = process.argv.slice(2);
function argValue(name) {
  const i = args.indexOf(name);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : null;
}
function die(message) {
  console.error(`\n✗ version-bump: ${message}\n`);
  process.exit(1);
}
function pass(message) {
  console.log(`\n✓ version-bump: ${message}\n`);
  process.exit(0);
}

const base = argValue("--base");
const head = argValue("--head");
const title = (argValue("--title") ?? "").trim();
const labels = (argValue("--labels") ?? "").trim();
if (!base || !head) die("missing --base or --head revision");

function git(argsLike) {
  return execFileSync("git", argsLike, { encoding: "utf8" }).trim();
}

function versionAt(rev) {
  try {
    const content = git(["show", `${rev}:Cargo.toml`]);
    const m = content.match(/^\[workspace\.package\]\s*\nversion = "([^"]+)"/m);
    if (!m) throw new Error("no [workspace.package] version in Cargo.toml");
    return m[1];
  } catch (err) {
    die(`cannot read the workspace version at ${rev} (${err.message})`);
  }
}

// The release-exempt paths — a PR touching only these must never carry a bump.
const EXEMPT = [/^docs\//, /\.md$/, /^\.github\//, /^scripts\//];

function classifyDelta(from, to) {
  const a = from.split(".").map(Number);
  const b = to.split(".").map(Number);
  if (b[0] > a[0]) return "major";
  if (b[0] < a[0]) return "none";
  if (b[1] > a[1]) return "minor";
  if (b[1] < a[1]) return "none";
  if (b[2] > a[2]) return "patch";
  return "none";
}

const BUCKET_LABEL = { patch: "release: patch", minor: "release: minor", major: "release: major" };

function bumpCommand(bucket, reason) {
  return (
    `node scripts/bump-version.mjs --bump ${bucket} --notes "One curated line for [Unreleased]"\n` +
    `      ${reason} (docs/RELEASING.md has the semver table)`
  );
}

// ---------------------------------------------------------------------------
// 1. Release-exempt paths only → always pass.

const changed = git(["diff", "--name-only", `${base}...${head}`]).split("\n").filter(Boolean);
if (changed.length === 0) pass("nothing changed since the merge-base");
const nonExempt = changed.filter((file) => !EXEMPT.some((re) => re.test(file)));
if (nonExempt.length === 0) {
  pass(
    `only release-exempt paths changed (${changed.map((f) => `\`${f}\``).join(", ")}) — no bump needed`
  );
}

// ---------------------------------------------------------------------------
// 2. Explicit opt-out.

const hasNoReleaseTitle = /\[no release\]/i.test(title);
const hasNoReleaseLabel = /(?:^|\s)no-release(?:\s|$)/.test(labels);
if (hasNoReleaseTitle || hasNoReleaseLabel) {
  pass(`PR opts out of a release (${hasNoReleaseTitle ? "`[no release]` in the title" : "`no-release` label"})`);
}

// ---------------------------------------------------------------------------
// 3. The version delta.

const baseVersion = versionAt(base);
const headVersion = versionAt(head);
const delta = classifyDelta(baseVersion, headVersion);
const declared = [...labels.matchAll(/release:\s*(patch|minor|major)/g)].map((m) => m[1]);
const declaredBucket = new Set(declared);
if (declaredBucket.size > 1) {
  die(`conflicting release labels: ${[...declaredBucket].map((b) => BUCKET_LABEL[b]).join(" and ")} — keep exactly one`);
}

// autoBump(bucket, reason): reached only when the gate would otherwise fail
// because the version never increased — the one case CI can fix on its own
// (issue #111). Prints the same human guidance as before (on a fork or the
// merge-queue ref that guidance is still the whole story), then the
// machine-readable payload version-bump.yml reads, and exits 3. The note is
// the PR title: bump-version.mjs strips the Conventional-Commits `type(scope):`
// prefix from it when it becomes the [Unreleased] bullet. The bucket honours a
// declared `release:` label when present, else the title's conventional type.
function autoBump(bucket, reason) {
  console.error(
    `\n✗ version-bump: ${reason}\n` +
      `  Run this in the PR:\n      ${bumpCommand(bucket, `suggested ${bucket} from the PR title / declared release: label`)}\n` +
      `  or, if this genuinely should not ship in a release, add "[no release]" to the PR title ` +
      `or set the no-release label.\n`
  );
  console.log(`AUTO_BUMP_BUCKET=${bucket}`);
  console.log(`AUTO_BUMP_NOTE=${title.replace(/\s*\n\s*/g, " ").trim()}`);
  process.exit(3);
}

if (declaredBucket.size === 1) {
  const [bucket] = declaredBucket;
  if (delta === bucket) {
    pass(`declared ${BUCKET_LABEL[bucket]} matches the bump ${baseVersion} → ${headVersion}`);
  }
  if (delta === "none") {
    // Declared a bucket but never actually bumped: the bucket is unambiguous,
    // so CI applies exactly that bucket.
    autoBump(
      bucket,
      `declared ${BUCKET_LABEL[bucket]} but the workspace version never increased (still ${headVersion} on the PR head)`
    );
  }
  die(
    `declared ${BUCKET_LABEL[bucket]} but the workspace version went ${baseVersion} → ${headVersion} ` +
      `(${delta} bump) since the merge-base.\n` +
      `  Fix one side — re-bump to match the label:\n      ${bumpCommand(bucket, `to match the ${BUCKET_LABEL[bucket]} label`)}\n` +
      `  or correct the label on the PR.`
  );
}

if (delta !== "none") {
  pass(`workspace version increased ${baseVersion} → ${headVersion} (${delta}) since the merge-base`);
}

// ---------------------------------------------------------------------------
// 4. Unlabeled and not bumped → the same auto-bumpable outcome.

const titleType = (title.match(/^([a-z][\w-]*)/i) || [])[1]?.toLowerCase();
const suggested = titleType === "feat" ? "minor" : titleType === "fix" ? "patch" : "patch";
autoBump(
  suggested,
  `${nonExempt.length} user-facing path(s) changed (${nonExempt.map((f) => `\`${f}\``).join(", ")}) ` +
    `but the workspace version never increased since the merge-base ` +
    `(${baseVersion} → ${headVersion})`
);
