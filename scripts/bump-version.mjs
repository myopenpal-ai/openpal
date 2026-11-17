#!/usr/bin/env node
// Version bumps are PR-driven: the author of a user-facing change decides the
// bucket, bumps the workspace version and adds their changelog line in the SAME
// PR, and CI (version-bump.yml) enforces that no releasable PR ships without a
// bump. This script makes that one reviewable edit — it rewrites every place
// the version lives — the root Cargo.toml `[workspace.package] version` (the
// single source all five crates read via `version.workspace = true`), the
// built-in plugin manifests (`plugins/*/plugin.json`, enumerated automatically
// so a new plugin can never be forgotten), the five workspace entries in
// `Cargo.lock` and `CHANGELOG.md`, then verifies the result. Full runbook:
// docs/RELEASING.md.
//
//   node scripts/bump-version.mjs --bump patch --notes "Added: …"  # PR author: bump + changelog line
//   node scripts/bump-version.mjs --bump minor                      # bump without a changelog note
//   node scripts/bump-version.mjs --target 0.2.0 --notes "…"        # force a specific version
//   node scripts/bump-version.mjs finalize                          # maintainer: [Unreleased] → [X.Y.Z]
//   node scripts/bump-version.mjs --bump patch --dry-run            # print the plan, write nothing
//   node scripts/bump-version.mjs --bump patch --force              # allow a dirty working tree
//
// The next version comes from the current workspace version, never from git
// history: `--bump patch|minor|major` applies the doc/RELEASING.md semver table
// to it, `--target X.Y.Z` pins it exactly. `finalize` (run at cut time, on an
// already-bumped main) renames `## [Unreleased]` into a dated `## [X.Y.Z]`
// section and opens a fresh `[Unreleased]` for the next cycle.
//
// Hand-rolled rather than release-please / cargo-release on purpose: the bump
// must stay a single, reviewable diff that touches exactly the files below,
// and this repo's tooling philosophy is small and opinionated. It needs only
// Node >= 18 and `git` on PATH. Run it from anywhere; it roots itself at the
// repository.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const CARGO_TOML = resolve(ROOT, "Cargo.toml");
const CARGO_LOCK = resolve(ROOT, "Cargo.lock");
const CHANGELOG = resolve(ROOT, "CHANGELOG.md");
const UPDATE_RS = resolve(ROOT, "crates/core/src/update.rs");

// The built-in plugin manifests — enumerated from plugins/, the same way the
// bundler's backstop test does (bundler/src/stage_plugins.rs
// `builtin_plugin_manifests_carry_the_workspace_version`), so a new plugin is
// picked up here automatically and can never drift. Each one's version field
// must track the workspace version or a release bundle carries a stale plugin
// version.
function builtinPluginManifests() {
  return readdirSync(resolve(ROOT, "plugins"), { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => resolve(ROOT, "plugins", entry.name, "plugin.json"))
    .filter((path) => existsSync(path))
    .sort();
}
const PLUGIN_MANIFESTS = builtinPluginManifests();

// The five workspace crates; Cargo.lock records each one's resolved version.
const WORKSPACE_CRATES = [
  "openpal-app",
  "openpal-bundler",
  "openpal-core",
  "openpal-relay",
  "openpal-sync-protocol",
];

const SEMVER_RE = /^\d+\.\d+\.\d+$/;
const BUCKETS = ["patch", "minor", "major"];

// ---------------------------------------------------------------------------
// CLI

const args = process.argv.slice(2);
function argValue(name) {
  const i = args.indexOf(name);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : null;
}
function argGiven(name) {
  return args.includes(name);
}

const bump = argValue("--bump");
const target = argValue("--target");
const notes = argValue("--notes");
const dryRun = args.includes("--dry-run");
const force = args.includes("--force");
// Positionals = anything that isn't a flag or the value of a value-taking flag.
const valueFlags = ["--bump", "--target", "--notes"];
const positionals = args.filter((a, i) => {
  if (a.startsWith("--")) return false;
  return !valueFlags.includes(args[i - 1]);
});
const finalize = positionals.includes("finalize");

const knownFlags = ["--bump", "--target", "--notes", "--dry-run", "--force"];
const badFlags = args.filter((a) => a.startsWith("--") && !knownFlags.includes(a));
if (badFlags.length > 0) die(`unknown flag: ${badFlags[0]}`);
if (argGiven("--bump") && bump === null) die("--bump needs a value: patch, minor or major");
if (argGiven("--target") && target === null) die("--target needs a value (e.g. --target 0.2.0)");
if (argGiven("--notes") && notes === null) die("--notes needs a value (the changelog line, in quotes)");

let mode;
if (bump !== null && target !== null) die("--bump and --target are mutually exclusive");
if (finalize && (bump !== null || target !== null)) die("finalize cannot be combined with --bump or --target");
if (finalize && positionals.length !== 1) die(`unexpected positional argument: ${positionals.find((p) => p !== "finalize")}`);
if (!finalize && positionals.length > 0) die(`unexpected positional argument: ${positionals[0]}`);
if (finalize) {
  mode = "finalize";
} else if (bump !== null) {
  mode = "bump";
} else if (target !== null) {
  mode = "target";
} else {
  die("nothing to do — specify one of --bump patch|minor|major, --target X.Y.Z, or finalize (see --help)");
}
if (notes !== null && mode === "finalize") {
  die("--notes makes no sense with finalize — finalize releases whatever notes the PRs already added");
}
if (process.argv.includes("-h") || process.argv.includes("--help")) {
  console.log(`Usage:
  node scripts/bump-version.mjs --bump <patch|minor|major> [--notes "..."] [--dry-run] [--force]
  node scripts/bump-version.mjs --target X.Y.Z       [--notes "..."] [--dry-run] [--force]
  node scripts/bump-version.mjs finalize              [--dry-run] [--force]
  --bump <bucket>  PR author: bump the next version from the current one
                   (patch 0.1.0 → 0.1.1, minor → 0.2.0, major → 1.0.0)
  --target X.Y.Z   release this exact version instead of computing it
  --notes "…"      append this curated line under [Unreleased] in CHANGELOG.md
  finalize         maintainer: rename [Unreleased] into a dated [X.Y.Z]
                   section and open a fresh [Unreleased] (version already bumped)
  --dry-run        print the plan without changing any file
  --force          run even with a dirty working tree (default: refuse)
See docs/RELEASING.md for the PR-driven release flow.`);
  process.exit(0);
}
if (mode !== "finalize" && target !== null && !SEMVER_RE.test(target)) {
  die(`--target must be a bare semver like 0.2.0 (got "${target}")`);
}

// ---------------------------------------------------------------------------
// Helpers

function git(args, opts = {}) {
  return execFileSync("git", args, { cwd: ROOT, encoding: "utf8", ...opts }).trim();
}

function die(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

function read(path) {
  return readFileSync(path, "utf8");
}

function write(path, content) {
  if (read(path) === content) return false;
  writeFileSync(path, content);
  return true;
}

function pathOf(path) {
  return path.replace(`${ROOT}/`, "");
}

function cargoVersion(cargoToml) {
  const m = cargoToml.match(/^\[workspace\.package\]\s*\nversion = "([^"]+)"/m);
  if (!m) die("no [workspace.package] version found in Cargo.toml");
  return m[1];
}

function compareSemver(a, b) {
  return a
    .split(".")
    .map(Number)
    .join(".")
    .localeCompare(b.split(".").map(Number).join("."), undefined, { numeric: true });
}

// The doc/RELEASING.md semver table applied to the *current* version — the PR
// author's decision, not something computed from git history.
function bumpBucket(current, bucket) {
  const [major, minor, patch] = current.split(".").map(Number);
  if (bucket === "major") return `${major + 1}.0.0`;
  if (bucket === "minor") return `${major}.${minor + 1}.0`;
  if (bucket === "patch") return `${major}.${minor}.${patch + 1}`;
  die(`--bump must be one of ${BUCKETS.join(", ")} (got "${bucket}")`);
}

// ---------------------------------------------------------------------------
// The edits — each a targeted text rewrite that fails loudly if the shape of
// the file changes, so a restructured file can never be silently skipped.

function setCargoVersion(cargoToml, next) {
  const re = /^(\[workspace\.package\]\s*\nversion = ")[^"]+(")/m;
  if (!re.test(cargoToml)) die("Cargo.toml [workspace.package] version line not found");
  return cargoToml.replace(re, `$1${next}$2`);
}

function setLockVersion(lock, crate, next) {
  const re = new RegExp(`(name = "${crate}"\\nversion = ")[^"]+(")`);
  if (!re.test(lock)) die(`Cargo.lock entry for ${crate} not found`);
  return lock.replace(re, `$1${next}$2`);
}

function setPluginVersion(json, next) {
  const re = /^(\s*"version"\s*:\s*")[^"]+(")/m;
  if (!re.test(json)) die('a plugin.json is missing its "version" field');
  return json.replace(re, `$1${next}$2`);
}

// The [Unreleased] block is CHANGELOG's first section (heading + body, up to
// the next "## [" heading or EOF). Extract it and return { body, tail }.
function unreleasedBlock(changelog) {
  const headerRe = /^## \[Unreleased\]\n/m;
  const m = changelog.match(headerRe);
  if (!m) die('CHANGELOG.md has no "## [Unreleased]" section — restore it');
  const bodyStart = m.index + m[0].length;
  const rest = changelog.slice(bodyStart);
  const nextHeading = rest.search(/^## \[/m);
  const bodyEnd = nextHeading === -1 ? rest.length : nextHeading;
  return {
    head: changelog.slice(0, bodyStart),
    body: rest.slice(0, bodyEnd),
    tail: rest.slice(bodyEnd),
  };
}

// Turn --notes into changelog lines: a raw phrase becomes a bullet, a phrase
// that already carries its own formatting (`- item`, `### Heading`, blank
// lines) is kept as the author wrote it. Changelog bullets drop the
// `type(scope): ` Conventional-Commits prefix — "### Added" is already the
// category.
function changelogLines(notes) {
  return notes.split("\n").map((line) => {
    const trimmed = line.trimEnd();
    if (/^[-*] |^#{1,6} |^$/.test(trimmed)) return trimmed;
    return `- ${stripConventionalPrefix(trimmed)}`;
  });
}

function stripConventionalPrefix(subject) {
  const m = subject.match(/^([a-zA-Z][\w-]*)(?:\([^)]*\))?!?:\s*/);
  if (!m) return subject;
  return subject.slice(m[0].length);
}

function appendChangelogNotes(changelog, notes) {
  const block = unreleasedBlock(changelog);
  // Drop the "(No notable changes yet.)" placeholder when the first note lands.
  const body = block.body.replace(/^\s*\(No notable changes yet\.\)\s*$/m, "").trimEnd();
  const lines = changelogLines(notes);
  if (lines.length === 0) die("--notes produced no changelog lines");
  const items = [body, ...lines].filter(Boolean).join("\n");
  return block.head + "\n" + items + "\n\n" + block.tail;
}

// Cut time: rename [Unreleased] into a dated [X.Y.Z] section (the version is
// already the workspace version — the last PR bumped it) and open a fresh,
// empty [Unreleased] above it. CHANGELOG stays newest-first.
function finalizeChangelog(changelog, version) {
  const block = unreleasedBlock(changelog);
  const released = block.body.replace(/^\s*\(No notable changes yet\.\)\s*$/m, "").trim();
  if (released === "") {
    console.warn("warning: [Unreleased] has no notes — nothing user-facing will appear in this release");
  }
  const today = new Date().toISOString().slice(0, 10);
  return (
    block.head +
    "\n(No notable changes yet.)\n\n" +
    `## [${version}] - ${today}\n\n` +
    (released === "" ? "(No notable changes yet.)" : released) +
    "\n\n" +
    block.tail
  );
}

// ---------------------------------------------------------------------------
// Plan

console.log(
  `\nOpenPal release bump — ${dryRun ? "DRY RUN (nothing written)" : "about to edit files"}\n`
);

const current = cargoVersion(read(CARGO_TOML));

if (mode === "bump" && !BUCKETS.includes(bump)) die(`--bump must be one of ${BUCKETS.join(", ")} (got "${bump}")`);

let next = null;
if (mode === "bump") {
  next = bumpBucket(current, bump);
} else if (mode === "target") {
  if (compareSemver(target, current) <= 0) {
    die(`--target ${target} is not newer than the current workspace version ${current}`);
  }
  next = target;
}

console.log(`current workspace version : ${current}`);
if (mode !== "finalize") {
  console.log(`next version               : v${next}${mode === "target" ? " (--target)" : ` (--bump ${bump})`}`);
  if (notes !== null) console.log(`changelog note(s)         : ${changelogLines(notes).length}`);
}

// Dirty-tree guard: the bump is meant to be the entire release/bump commit, so
// it must start from a clean tree.
if (!dryRun && !force) {
  const dirty = git(["status", "--porcelain"]);
  if (dirty !== "") {
    die(
      `working tree is not clean — commit or stash first (or pass --force):\n` +
        dirty.split("\n").map((l) => `  ${l}`).join("\n")
    );
  }
}

if (dryRun) {
  if (mode === "finalize") {
    console.log("\nWould update:");
    console.log(`  - ${pathOf(CHANGELOG)}`);
    console.log(
      `\nWould turn [Unreleased] into:\n\n## [${current}] - ${new Date().toISOString().slice(0, 10)}\n`
    );
    console.log("Dry run only — no files were changed.\n");
  } else {
    const files = [pathOf(CARGO_TOML), pathOf(CARGO_LOCK), ...PLUGIN_MANIFESTS.map(pathOf), pathOf(CHANGELOG)];
    console.log("\nWould update:");
    for (const f of files) console.log(`  - ${f}`);
    if (notes !== null) {
      console.log(`\nChangelog lines that would be appended under [Unreleased]:`);
      for (const line of changelogLines(notes)) console.log(`  ${line}`);
    }
    console.log("Dry run only — no files were changed.\n");
  }
  process.exit(0);
}

// ---------------------------------------------------------------------------
// Apply.

let edits = 0;
if (mode !== "finalize") {
  edits += write(CARGO_TOML, setCargoVersion(read(CARGO_TOML), next)) ? 1 : 0;
  for (const crate of WORKSPACE_CRATES) {
    edits += write(CARGO_LOCK, setLockVersion(read(CARGO_LOCK), crate, next)) ? 1 : 0;
  }
  for (const manifest of PLUGIN_MANIFESTS) {
    edits += write(manifest, setPluginVersion(read(manifest), next)) ? 1 : 0;
  }
}
if (notes !== null) {
  edits += write(CHANGELOG, appendChangelogNotes(read(CHANGELOG), notes)) ? 1 : 0;
} else if (mode === "finalize") {
  edits += write(CHANGELOG, finalizeChangelog(read(CHANGELOG), current)) ? 1 : 0;
}

// Verify the postconditions the release depends on — only when a version was
// written (finalize leaves the version files alone by design).
if (mode !== "finalize") verify(next);

console.log(
  mode === "finalize"
    ? `\n✓ finalized the changelog for v${current} (${edits} file(s) edited): CHANGELOG.md — version files were already bumped by the PRs`
    : `\n✓ bumped to v${next} (${edits} file(s) edited): Cargo.toml, Cargo.lock, ${PLUGIN_MANIFESTS.length} plugin manifests${notes !== null ? ", CHANGELOG.md" : ""}\n`
);

console.log("Next steps (docs/RELEASING.md):");
if (mode === "finalize") {
  console.log(`  1. git commit -m "chore(release): finalize CHANGELOG for v${current}"`);
  console.log(`  2. git tag v${current} && git push origin v${current}`);
  console.log("  3. wait for the `portable` workflow's draft release, then:");
  console.log(`     gh release edit v${current} --draft=false`);
} else {
  console.log(`  1. commit ${notes !== null ? "the bump and changelog line" : "the bump"} in the SAME PR as the change — `);
  console.log(`     git commit -m "<conventional-type>: what changed"`);
  console.log("  2. the version-bump check passes, squash-merge the PR");
  console.log("  3. repeat per PR; at cut time a maintainer runs `node scripts/bump-version.mjs finalize`\n");
}

function verify(next) {
  if (cargoVersion(read(CARGO_TOML)) !== next) {
    die("verification failed: Cargo.toml workspace version did not update");
  }
  const lock = read(CARGO_LOCK);
  for (const crate of WORKSPACE_CRATES) {
    const re = new RegExp(`name = "${crate}"\\nversion = "${next}"`);
    if (!re.test(lock)) die(`verification failed: Cargo.lock ${crate} is not ${next}`);
  }
  for (const manifest of PLUGIN_MANIFESTS) {
    const expected = `"version": "${next}"`;
    if (!read(manifest).includes(expected)) {
      die(`verification failed: ${pathOf(manifest)} is not version ${next}`);
    }
  }
  // The updater tests derive their fixtures from CARGO_PKG_VERSION (no
  // literals), so a stale literal here means someone reintroduced a hand
  // mirror — the release must not ship that.
  const update = read(UPDATE_RS);
  if (update.includes(`Version::parse("${current}")`)) {
    die(
      `verification failed: ${pathOf(UPDATE_RS)} still hardcodes the previous ` +
        `version (${current}) — its tests must derive from CARGO_PKG_VERSION`
    );
  }
}
