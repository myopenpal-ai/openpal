#!/usr/bin/env node
// `npm run dev` (tauri dev) on a throwaway first-run profile, so the onboarding
// flow shows on every run without touching your real OpenPal data.
//
//   npm run dev:onboarding                   # reset the profile, then launch
//   npm run dev:onboarding -- --wipe-models  # ...and delete its downloaded models too
//
// Why a fresh profile rather than a flag: `onboardingCompleted` is a row in the
// encrypted app DB (crates/core/src/settings.rs) whose key only exists inside the
// running backend, so there is nothing to edit from a shell. A data dir with no DB
// has no such row, and `Settings::default()` says `onboarding_completed: false` —
// exactly a first launch.
//
// How: `OPENPAL_PORTABLE=1` (crates/app/src/state.rs, `resolve_data_dir`) keeps all
// app state in `<executable dir>/data`. Under `tauri dev` that is
// `<cargo target dir>/debug/data`: gitignored, one per checkout, nowhere near your
// real data. This script empties it and runs the repo's own `npm run dev` with the
// variable set. Downloaded models survive unless `--wipe-models` — the same call the
// app's own factory reset makes (crates/core/src/boot/factory_reset.rs): a
// multi-gigabyte download isn't worth repeating just to see the welcome screen.
//
// What this profile can't show, and what never to do in it, is printed at launch.
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Where Vite listens: pinned (`strictPort`) in ui/vite.config.ts and named by
// `devUrl` in crates/app/tauri.conf.json.
const DEV_PORT = 5173;
const USAGE = "usage: npm run dev:onboarding [-- --wipe-models]";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  console.error(`dev:onboarding: ${message}`);
  process.exit(1);
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log(USAGE);
  process.exit(0);
}
// Without the `--` separator npm swallows the flag and hands it on as this env var.
const wipeModels = args.includes("--wipe-models") || process.env.npm_config_wipe_models === "true";
const unknown = args.filter((arg) => arg !== "--wipe-models");
if (unknown.length > 0) {
  console.error(`dev:onboarding: unknown argument: ${unknown.join(" ")}\n${USAGE}`);
  process.exit(2);
}

// What a connect error looks like when nothing is listening there; any other
// outcome (a reset from a full accept queue, a timeout) means something is.
const NOT_LISTENING = new Set(["ECONNREFUSED", "EADDRNOTAVAIL", "EAFNOSUPPORT", "ENETUNREACH"]);

// True if anything accepts connections on `port`, on either loopback family
// (Vite binds `localhost`, which can resolve to either).
function portInUse(port) {
  const probe = (host) =>
    new Promise((resolve) => {
      const socket = net.connect({ port, host });
      socket.setTimeout(1000, () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("error", (err) => resolve(!NOT_LISTENING.has(err.code)));
    });
  return Promise.all([probe("127.0.0.1"), probe("::1")]).then((hits) => hits.some(Boolean));
}

// Asks cargo rather than assuming `<repo>/target` (CARGO_TARGET_DIR and
// `.cargo/config.toml` apply), from `crates/app` — where `npm run dev` runs cargo.
function cargoTargetDir() {
  let target;
  try {
    const metadata = execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
      cwd: path.join(repoRoot, "crates", "app"),
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "inherit"],
    });
    target = JSON.parse(metadata).target_directory;
  } catch (err) {
    fail(
      err.code === "ENOENT"
        ? "cargo isn't on PATH — is Rust installed? See README → Development."
        : `couldn't ask cargo for its target dir (${err.message}).`,
    );
  }
  if (typeof target !== "string" || !path.isAbsolute(target)) {
    fail(`cargo reported an unusable target dir (${JSON.stringify(target)}).`);
  }
  return target;
}

// The pid in `<data>/.lock` (crates/core/src/boot/lock.rs) if that process is
// still running: an OpenPal still on this profile — say, orphaned when its
// terminal was killed — must not have its database deleted from under it.
function liveLockHolder(dataDir) {
  let pid;
  try {
    pid = Number.parseInt(fs.readFileSync(path.join(dataDir, ".lock"), "utf8"), 10);
  } catch {
    return null;
  }
  if (!Number.isInteger(pid) || pid <= 0) return null;
  try {
    process.kill(pid, 0); // existence check only; sends nothing
    return pid;
  } catch (err) {
    return err.code === "EPERM" ? pid : null;
  }
}

// Empties the profile so the next launch is a first launch; `models/` survives
// unless `--wipe-models`. Symlinks are unlinked, never followed.
function resetProfile(dataDir) {
  try {
    const stat = fs.lstatSync(dataDir, { throwIfNoEntry: false });
    if (!stat) return;
    if (stat.isSymbolicLink()) {
      fail(`${dataDir} is a symlink — refusing to empty whatever it points at.`);
    }
    for (const entry of fs.readdirSync(dataDir)) {
      if (entry === "models" && !wipeModels) continue;
      const entryPath = path.join(dataDir, entry);
      if (fs.lstatSync(entryPath).isSymbolicLink()) fs.unlinkSync(entryPath);
      else fs.rmSync(entryPath, { recursive: true, force: true });
    }
  } catch (err) {
    fail(`couldn't reset ${dataDir} (${err.message}) — fix its permissions or delete it by hand.`);
  }
}

// The app runs the same probe (crates/app/src/state.rs, `is_writable`) and, when
// it fails, quietly falls back to your REAL data dir while staying portable.
function assertWritable(dataDir) {
  try {
    fs.mkdirSync(dataDir, { recursive: true });
    const probe = path.join(dataDir, ".openpal-write-test");
    fs.writeFileSync(probe, "");
    fs.rmSync(probe);
  } catch (err) {
    fail(`${dataDir} isn't writable (${err.message}) — OpenPal would fall back to your real data folder.`);
  }
}

const hasEntries = (dir) => {
  try {
    return fs.readdirSync(dir).length > 0;
  } catch {
    return false;
  }
};

// A second dev server can't bind the port (`strictPort`), so `tauri dev` would
// either fail or show the first one's frontend — often another checkout's.
if (await portInUse(DEV_PORT)) {
  fail(
    `something is already listening on :${DEV_PORT} — likely an \`npm run dev\` from this or another checkout.\n` +
      "`tauri dev` would fail or show that server's frontend instead of this checkout's.\n" +
      `Find it with \`lsof -nP -iTCP:${DEV_PORT} -sTCP:LISTEN\`, stop it, then run this again.`,
  );
}

const targetDir = cargoTargetDir();
const dataDir = path.join(targetDir, "debug", "data");

const holder = liveLockHolder(dataDir);
if (holder !== null) {
  fail(
    `an OpenPal (pid ${holder}) is still running on ${dataDir} — quit it first.\n` +
      "If that pid isn't OpenPal, delete the stale .lock file in that folder and run this again.",
  );
}
resetProfile(dataDir);
assertWritable(dataDir);

const keepingModels = !wipeModels && hasEntries(path.join(dataDir, "models"));
const engine = process.platform === "win32" ? "llama-server.exe" : "llama-server";
const engineStaged = [
  path.join(repoRoot, "crates", "app", "resources", "llama"),
  path.join(targetDir, "debug", "llama"),
].some((dir) => fs.existsSync(path.join(dir, engine)));

console.log(`dev:onboarding — throwaway first-run profile at ${dataDir}`);
console.log("  reset each time you run this (a Rust-triggered relaunch keeps it); your real OpenPal data isn't used");
if (keepingModels) {
  console.log("  kept its downloaded models — pass --wipe-models for a true first run");
}
console.log("  portable mode: no recovery-code screen before onboarding (so the PIN offer's");
console.log("  recovery-code line isn't true here either), and no Plugins tab");
if (!engineStaged) {
  console.log("  no llama-server staged: a downloaded model can't start, so the PIN and ready");
  console.log("  scenes are unreachable — run `cargo run -p openpal-bundler -- fetch-engine`");
}
console.log("  NEVER use Settings → Factory reset here: it also deletes the OS-keychain key");
console.log("  your real OpenPal install shares");
console.log("  Ctrl+C stops it\n\n> npm run dev   (OPENPAL_PORTABLE=1)\n");

const options = {
  cwd: repoRoot,
  stdio: "inherit",
  env: {
    ...process.env,
    OPENPAL_PORTABLE: "1",
    // The account/cloud backend, pointed at a closed port unless you set your own
    // stub. A real signed-in session on this machine keeps its refresh token in the
    // shared OS keychain; left alone, this profile would send it off at boot and
    // cache the account (crates/core/src/account.rs). The refresh just fails quietly.
    OPENPAL_ACCOUNT_API: process.env.OPENPAL_ACCOUNT_API ?? "http://127.0.0.1:1",
  },
};
// npm is `npm.cmd` on Windows, which needs a shell (and then one command string).
const child =
  process.platform === "win32"
    ? spawn("npm run dev", { ...options, shell: true })
    : spawn("npm", ["run", "dev"], options);
// Ctrl+C reaches the whole foreground process group, so npm and tauri shut
// themselves down; just wait for them rather than exiting first and leaving the
// shell prompt tangled up in their teardown output. (Signalling only this process
// doesn't stop the tree — use Ctrl+C.)
process.on("SIGINT", () => {});
for (const signal of ["SIGTERM", "SIGHUP"]) process.on(signal, () => child.kill(signal));
child.on("error", (err) => fail(`couldn't start \`npm run dev\`: ${err.message}`));
child.on("exit", (code, signal) => process.exit(code ?? 128 + os.constants.signals[signal]));
