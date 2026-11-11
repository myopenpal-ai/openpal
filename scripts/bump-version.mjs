#!/usr/bin/env node
// Cut a release in one reviewable step: apply the semver rule to the commits
// since the last tag, then bump every place the version lives — the root
// Cargo.toml `[workspace.package] version` (the single source all five crates
// read via `version.workspace = true`), the five built-in plugin manifests
// (`plugins/*/plugin.json`), the five workspace entries in `Cargo.lock` and
// `CHANGELOG.md`. Full runbook: docs/RELEASING.md.
//
//   node scripts/bump-version.mjs                    # next version, from conventional commits
//   node scripts/bump-version.mjs --target 0.2.0     # force a specific version
//   node scripts/bump-version.mjs --dry-run          # print the plan, write nothing
//   node scripts/bump-version.mjs --force            # allow a dirty working tree
//
// Hand-rolled rather than release-please / cargo-release on purpose: the bump
// must stay a single, reviewable diff that touches exactly the files below,
// and this repo's tooling philosophy is small and opinionated. It needs only
// Node >= 18 and `git` on PATH. Run it from anywhere; it roots itself at the
// repository.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const CARGO_TOML = resolve(ROOT, "Cargo.toml");
const CARGO_LOCK = resolve(ROOT, "Cargo.lock");
const CHANGELOG = resolve(ROOT, "CHANGELOG.md");
const UPDATE_RS = resolve(ROOT, "crates/core/src/update.rs");

// The five built-in plugin manifests — the version field of each must track
// the workspace version or a release bundle carries a stale plugin version.
const PLUGIN_MANIFESTS = [
  "hf-browser",
  "smart-model-router",
  "speech",
  "web-search",
  "writing-tools",
].map((id) => resolve(ROOT, "plugins", id, "plugin.json"));

// The five workspace crates; Cargo.lock records each one's resolved version.
const WORKSPACE_CRATES = [
  "openpal-app",
  "openpal-bundler",
  "openpal-core",
  "openpal-relay",
  "openpal-sync-protocol",
];

const SEMVER_RE = /^\d+\.\d+\.\d+$/;
const CONVENTIONAL_TYPES = new Set([
  "feat",
  "fix",
  "chore",
  "docs",
  "refactor",
  "perf",
  "test",
  "build",
  "ci",
  "style",
  "revert",
  "deps",
]);

// ---------------------------------------------------------------------------
// CLI

const args = process.argv.slice(2);
function argValue(name) {
  const i = args.indexOf(name);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : null;
}
const target = argValue("--target");
if (args.includes("--target") && target === null) die("--target needs a value (e.g. --target 0.2.0)");
const dryRun = args.includes("--dry-run");
const force = args.includes("--force");
const badFlags = args.filter(
  (a) => a.startsWith("--") && !["--target", "--dry-run", "--force"].includes(a)
);
if (badFlags.length > 0) die(`unknown flag: ${badFlags[0]}`);
if (args.some((a) => !a.startsWith("--") && a !== target)) die("unexpected positional argument");
if (process.argv.includes("-h") || process.argv.includes("--help")) {
  console.log(`Usage: node scripts/bump-version.mjs [--target X.Y.Z] [--dry-run] [--force]
  --target X.Y.Z   release this exact version instead of computing it
  --dry-run        print the plan without changing any file
  --force          run even with a dirty working tree (default: refuse)
See docs/RELEASING.md for the full release runbook.`);
  process.exit(0);
}
if (target !== null && !SEMVER_RE.test(target)) {
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

function lastTag() {
  try {
    return git(["describe", "--tags", "--abbrev=0"], { stdio: ["ignore", "pipe", "ignore"] });
  } catch {
    return null; // no tags yet — the repo has never released
  }
}

function commitsInScope(tag) {
  // `git log <last-tag>..HEAD` with nothing between prints nothing but still
  // exits 0; `git log HEAD` (no tag) sweeps the whole history for the first
  // release. `--no-merges`: the repo merges squash-only, so every PR lands as
  // one conventional-commits commit and the occasional historical merge
  // commit is just noise ("Merge pull request #N …").
  const range = tag ? `${tag}..HEAD` : "HEAD";
  const raw = git(["log", "--no-merges", `--format=%H%x1f%s%x1f%b%x1e`, range]);
  return raw
    .split("\x1e")
    .filter(Boolean)
    .map((record) => {
      const [hash, subject, body = ""] = record.split("\x1f");
      return { hash, subject, body };
    });
}

// Conventional-Commits classification (CONTRIBUTING.md). "breaking" wins over
// everything; otherwise feat/fix drive the bump and everything else (chore,
// docs, refactor, perf, test, build, ci, style, revert, ...) only feeds the
// changelog.
function classify(commit) {
  const breaking =
    /!:/.test(commit.subject) || /^BREAKING[ -]CHANGE:/m.test(commit.body);
  if (breaking) return "breaking";
  const type = typeOf(commit.subject);
  if (type === "feat") return "feat";
  if (type === "fix") return "fix";
  return "other";
}

function typeOf(subject) {
  const m = subject.match(/^([a-zA-Z][\w-]*)(?:\([^)]*\))?!?:\s*/);
  if (!m) return null;
  const type = m[1].toLowerCase();
  return CONVENTIONAL_TYPES.has(type) ? type : null;
}

// Changelog bullets drop the `type(scope): ` prefix — "## Fixed" is already
// the category, so "(core): handle X" would be redundant.
function stripConventionalPrefix(subject) {
  const m = subject.match(/^([a-zA-Z][\w-]*)(?:\([^)]*\))?!?:\s*/);
  if (!m) return subject;
  return CONVENTIONAL_TYPES.has(m[1].toLowerCase()) ? subject.slice(m[0].length) : subject;
}

function nextFromCommits(current, kinds) {
  const [major, minor, patch] = current.split(".").map(Number);
  if (kinds.breaking > 0) return `${major + 1}.0.0`;
  if (kinds.feat > 0) return `${major}.${minor + 1}.0`;
  if (kinds.fix > 0) return `${major}.${minor}.${patch + 1}`;
  return null;
}

function compareSemver(a, b) {
  return a
    .split(".")
    .map(Number)
    .join(".")
    .localeCompare(b.split(".").map(Number).join("."), undefined, { numeric: true });
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

function changelogVersionSection(groups) {
  const order = [
    ["breaking", "### Breaking changes"],
    ["feat", "### Added"],
    ["fix", "### Fixed"],
    ["other", "### Changed"],
  ];
  const blocks = [];
  for (const [key, heading] of order) {
    const items = groups[key];
    if (items.length === 0) continue;
    blocks.push(heading, ...items.map((c) => `- ${stripConventionalPrefix(c.subject)}`));
  }
  return blocks.join("\n") + "\n";
}

// Replace the whole "## [Unreleased]" block (heading + body, up to the next
// "## [" heading or EOF) with a fresh placeholder and the new release section
// beneath it — CHANGELOG stays newest-first.
function setChangelog(changelog, next, section) {
  const headerRe = /^(## \[Unreleased\]\n)/m;
  const m = changelog.match(headerRe);
  if (!m) die('CHANGELOG.md has no "## [Unreleased]" section — restore it');
  const bodyStart = m.index + m[0].length;
  const rest = changelog.slice(bodyStart);
  const nextHeading = rest.search(/^## \[/m);
  const bodyEnd = nextHeading === -1 ? rest.length : nextHeading;
  const tail = rest.slice(bodyEnd);
  const today = new Date().toISOString().slice(0, 10);
  return (
    changelog.slice(0, bodyStart) +
    `\n(No notable changes yet.)\n\n` +
    `## [${next}] - ${today}\n\n` +
    section +
    "\n" +
    tail
  );
}

// ---------------------------------------------------------------------------
// Main

console.log(
  `\nOpenPal release bump — ${dryRun ? "DRY RUN (nothing written)" : "about to edit files"}\n`
);

const current = cargoVersion(read(CARGO_TOML));
const tag = lastTag();
const commits = commitsInScope(tag);

const kinds = { breaking: 0, feat: 0, fix: 0, other: 0 };
const groups = { breaking: [], feat: [], fix: [], other: [] };
for (const commit of commits) {
  const kind = classify(commit);
  kinds[kind] += 1;
  groups[kind].push(commit);
}

if (commits.length === 0) {
  die(
    tag
      ? `no commits since ${tag} — nothing to release`
      : "no commits found in this repository"
  );
}

let next;
if (target !== null) {
  next = target;
  if (tag !== null && compareSemver(next, tag.replace(/^v/, "")) <= 0) {
    die(`--target ${next} is not newer than the last tag ${tag}`);
  }
  if (next !== current && compareSemver(next, current) < 0) {
    die(`--target ${next} is older than the current workspace version ${current}`);
  }
} else {
  if (tag === null) {
    die(
      `no git tags exist yet — the first release needs an explicit --target ` +
        `(e.g. --target 0.1.0 to tag the current state); see docs/RELEASING.md`
    );
  }
  next = nextFromCommits(current, kinds);
  if (next === null) {
    die(
      `no version bump: the ${commits.length} commit(s) since ${tag} contain no ` +
        `feat, fix or breaking change (they are ${kinds.other} chore/docs/refactor/...). ` +
        `Cut a release only for releasable changes, per docs/RELEASING.md`
    );
  }
}

console.log([
  `current workspace version : ${current}`,
  tag ? `last tag                  : ${tag}` : "last tag                  : (none — first release)",
  `commits in scope           : ${commits.length} (${kinds.breaking} breaking, ${kinds.feat} feat, ${kinds.fix} fix, ${kinds.other} other)`,
  `next version               : v${next}${target !== null ? " (--target)" : ""}`,
].join("\n"));

// Dirty-tree guard: the bump is meant to be the entire release commit, and the
// changelog entries are generated from the committed history — so it must
// start from a clean tree.
if (!dryRun && !force) {
  const dirty = git(["status", "--porcelain"]);
  if (dirty !== "") {
    die(
      `working tree is not clean — commit or stash first (or pass --force):\n` +
        dirty.split("\n").map((l) => `  ${l}`).join("\n")
    );
  }
}

const section = changelogVersionSection(groups);

if (dryRun) {
  const files = [pathOf(CARGO_TOML), pathOf(CARGO_LOCK), ...PLUGIN_MANIFESTS.map(pathOf), pathOf(CHANGELOG)];
  console.log("\nWould update:");
  for (const f of files) console.log(`  - ${f}`);
  console.log(`\nChangelog section that would be written:\n\n## [${next}] - ${new Date().toISOString().slice(0, 10)}\n`);
  console.log(section);
  console.log("Dry run only — no files were changed.\n");
  process.exit(0);
}

if (tag === null) {
  console.log(
    `\nnote: no prior tag — the changelog section covers the full commit history. ` +
      `Trim it to a user-facing summary before committing.`
  );
}

// Apply.
let edits = 0;
edits += write(CARGO_TOML, setCargoVersion(read(CARGO_TOML), next)) ? 1 : 0;
for (const crate of WORKSPACE_CRATES) {
  edits += write(CARGO_LOCK, setLockVersion(read(CARGO_LOCK), crate, next)) ? 1 : 0;
}
for (const manifest of PLUGIN_MANIFESTS) {
  edits += write(manifest, setPluginVersion(read(manifest), next)) ? 1 : 0;
}
edits += write(CHANGELOG, setChangelog(read(CHANGELOG), next, section)) ? 1 : 0;

// Verify the postconditions the release depends on.
verify(next);

console.log(
  `\n✓ bumped to v${next} (${edits} file(s) edited): Cargo.toml, Cargo.lock, 5 plugin manifests, CHANGELOG.md\n`
);
console.log("Next steps (docs/RELEASING.md):");
console.log(`  1. git commit -m "chore(release): v${next}"  (single reviewable commit)`);
console.log("  2. open the release PR, get the required checks green, squash-merge");
console.log(`  3. git tag v${next} && git push origin v${next}`);
console.log("  4. wait for the `portable` workflow's draft release, then:");
console.log(`     gh release edit v${next} --draft=false\n`);

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
