# Multi-device sync — conversations across desktop and (future) mobile

This document is the plan for letting a user's conversations, projects, and
settings follow them across every device running OpenPal — today's desktop
app on a second machine, and eventually the Android/iOS apps that don't exist
yet. It complements [`ARCHITECTURE.md`](../../ARCHITECTURE.md) (read that first
for the Tauri shell → `openpal-core` → `llama-server` subprocess layout and
the "Data on disk" / crypto sections it documents) and follows the same
verified-facts discipline as [`DISK_OFFLOAD.md`](DISK_OFFLOAD.md).

**Revision (2026-08-28):** the first version of this document adopted
architecture C (bring-your-own cloud folder). After surveying how comparable
shipping products actually solve this — Standard Notes, Anytype, Obsidian
Sync, Joplin, AFFiNE, Logseq, Bitwarden, Signal — the decision was revised to
**B (a minimal, blind, self-hostable relay)**. The original C-vs-B tradeoff
analysis below is kept, not deleted, because the reasoning still matters: B
is adopted with its eyes open about the cost C was chosen to avoid.

## Problem statement

OpenPal keeps every conversation in a single local SQLCipher database,
`app.sqlite` (`openpal_core::app_db`), keyed by a per-install Data Encryption
Key (DEK) that is minted once, sealed in the OS keychain, and never leaves
the machine (`crates/app/src/bootstrap.rs::mint_or_unlock`,
`crates/core/src/crypto/key.rs:25-29`). That is exactly right for a
single-device, local-first product — but it means a user's chat history,
projects, custom models, and settings are stranded on whichever machine
created them. There is a one-way plaintext export (`export_data`,
`crates/app/src/commands.rs:3761`) and nothing that reads it back in. No
sync, no second-device story, no path to the Android/iOS apps that don't
exist yet but are clearly where this product is headed.

This plan proposes an architecture for moving a user's data between their
own devices without OpenPal (or a self-hosting operator) ever seeing
plaintext, and without contradicting the project's beginner-first/mono-model
design philosophy.

## Design review

### Stage 1 — four architectures evaluated internally

Four independent designs were evaluated end to end (pairing/key model,
transport, sync unit/conflict handling, threat model, platform
prerequisites) and scored by three independent judges across
`beginner_first_fit`, `local_first_purity`, `security_soundness`,
`engineering_cost_and_risk`, and `cross_platform_reality`:

- **A — LAN-only local pairing** (AirDrop model): mDNS discovery + direct
  mTLS, PAKE-hardened ephemeral pairing code, zero infrastructure.
- **B — OpenPal-operated blind E2E relay** (Signal/iMessage model): a
  OpenPal-run store-and-forward WebSocket relay that only ever sees
  ciphertext and HKDF-derived opaque mailbox IDs.
- **C — Bring-your-own cloud storage** (Syncthing/Obsidian-folder model):
  OpenPal writes an encrypted changelog into a folder the user already syncs
  (iCloud Drive, Dropbox, Syncthing, WebDAV); the OS-level sync client does
  all byte transport, OpenPal runs nothing.
- **D — P2P over rendezvous-only signaling** (Tailscale/iroh model): a
  content-blind relay/discovery network (iroh, QUIC) does NAT traversal;
  payloads are sync-key-encrypted before they ever reach it.

Two of three judges ranked **C first overall**; the third ranked it last on
security grounds alone (no integrity/anti-rollback guarantee against a
misbehaving sync provider) while still scoring it highest on
`engineering_cost_and_risk` and `local_first_purity`. B scored highest on
raw `security_soundness` from all three judges but lowest on
`engineering_cost_and_risk` — the tradeoff this revision accepts.

**Why C was picked first:** it adds zero OpenPal-operated infrastructure —
no server to build, fund, defend, or answer legal requests about. For a
single-maintainer local-first project, that was the deciding factor over B's
stronger raw cryptography.

### Stage 2 — real-world precedent check

Before committing, a second research pass looked at how eight comparable
shipping products actually solve multi-device sync: Standard Notes, Anytype,
Obsidian Sync, Joplin, AFFiNE, Logseq, Bitwarden, and Signal.

| App | Architecture | Pairing/key model |
|---|---|---|
| Standard Notes | **B**, self-host escape hatch | Password re-derives root key locally (Argon2id → XChaCha20-Poly1305); server is an explicit "dumb data-store" |
| Anytype | **B** (+A for LAN peers), self-host escape hatch | 12-word BIP-39 mnemonic re-entered/QR-scanned per device; no email/account — identity is key-based |
| Obsidian Sync | **B**, no self-host, server sees metadata | Per-vault password (scrypt→HKDF→AES-256-GCM); content is E2E, file paths/timestamps aren't |
| Joplin | **C primary**, B (Joplin Cloud) optional paid tier | Password decrypts a synced, already-encrypted master key; E2EE opt-in, off by default |
| AFFiNE | **B**, self-host escape hatch, *not* E2E | Yjs/CRDT over a WebSocket relay |
| Logseq | **C (legacy)** → **B (current Sync/RTC)** | Started on git/Syncthing, deliberately added a vendor relay once the project matured |
| Bitwarden | **B**, self-host escape hatch, zero-knowledge | PBKDF2/Argon2id-derived master key; "log in with device" via ephemeral keypair + fingerprint confirmation |
| Signal | **B**, no self-host, mandatory relay | QR-linked device gets a provisioning address; primary device encrypts account info to it through the relay (X3DH/Double Ratchet) |

**The pattern:** six of eight run vendor-operated infrastructure as their
primary path, and four of those six ship a self-host escape hatch. Pure
C-as-flagship is really just Joplin's free tier and old Logseq — and
Logseq's own trajectory (years on C-only, then a deliberate move to a
vendor relay, then a second, more real-time layer) is direct evidence for
"serious sync eventually outgrows a folder," not hypothetical. The apps
that needed B were pushed there by pressures OpenPal's own roadmap already
names: **mobile clients need reliable background sync** (the reason Logseq
and Signal both run relays; a folder-poll model degrades badly under
mobile OS background-execution limits — see "Per-platform prerequisites"),
and a delivery guarantee that doesn't depend on trusting a third-party
cloud provider's file-sync semantics.

> **Adopt B — a minimal, blind, self-hostable relay, modeled on Standard
> Notes'/Bitwarden's "dumb data store" pattern, with OpenPal operating a
> default instance and self-hosting documented as a first-class escape
> hatch (never a required trust relationship).**
>
> Keep C's per-conversation append-only sync-unit design and its LWW +
> tombstone + conflict-side-file conflict model — nothing about *how data is
> shaped* changes, only *how bytes move between devices*. Reuse Phase 0
> (`import_data`/`export_data`) unchanged as the zero-network bootstrap and
> permanent manual fallback. Upgrade the pairing design past the original
> B proposal's shared-code approach to an ephemeral-keypair,
> human-confirmed flow modeled on Bitwarden/Signal (see "Locked decisions").
> Defer A. Reject D (its "no server" pitch didn't hold up under research —
> see the original analysis below — but its Phase 0 idea is kept).

**Why B now, specifically:** the motivating use case for this whole document
is desktop-to-Android sync. Mobile is exactly the case where C's
folder-poll model is weakest (see "Per-platform prerequisites" — mobile OSes
suspend background file access aggressively) and exactly the case where
every researched precedent that ships a real mobile client (Signal,
Bitwarden, Anytype, current Logseq) runs a relay. Building C first and
migrating to B later would mean building the weaker design for the harder
platform on purpose. B is adopted now, before Phase 5's mobile bring-up,
specifically so mobile doesn't inherit a transport that's already known to
fit it poorly.

**What's kept from the original analysis (still correct, unaffected by the
B/C reversal):**

- **vs. A (LAN-only):** still true that mDNS doesn't survive VLANs/guest
  Wi-Fi/cellular, and it's not an ongoing-sync answer. Deferred, not
  rejected — see "Deferred" below.
- **vs. D (P2P rendezvous):** still true that D's "no server" pitch doesn't
  actually hold (it leans on a public relay and concedes it'll eventually
  need a self-hosted one for privacy — the same server-ownership question
  B answers directly instead of backing into). D's Phase-0 idea
  (`import_data` pairing `export_data`) is kept as this plan's actual
  Phase 0, unchanged.
- **B's original honest cost is still real and is being accepted, not
  waved away:** OpenPal (or a self-hosting operator) now runs, funds, and
  defends an always-on service — uptime, abuse/DDoS handling, storage for
  queued ciphertext, TLS, and an incident/subpoena story. See "Ops /
  non-engineering considerations" below — this is a genuine new kind of
  commitment for a local-first project and is called out explicitly rather
  than absorbed silently into "just ship the code."

**Deferred, not rejected — A (LAN-only pairing):** still worth building
later as a same-room "quick pair" convenience layered on top of B — a live
QR/PAKE handshake over mDNS is a nicer same-room pairing UX than the
relay-mediated flow, for the specific case where both devices are already
next to each other. Not the answer for ongoing sync or remote pairing.

**Rejected — D (P2P rendezvous):** the transport is rejected for the reason
given above; its Phase 0 idea lives on as this plan's Phase 0 regardless.

**Superseded, not deleted — C (bring-your-own cloud folder):** demoted from
primary transport to **documented fallback design**, not built as a live
path in v1. If a future phase finds the relay's ops burden is unsustainable
for a small team, C's shard-file/folder-transport design (preserved in this
document's git history) is a ready-made no-infrastructure alternative — the
same demotion this document originally gave B, now reversed.

### Locked decisions

| Decision | Choice |
|---|---|
| **Transport (v1)** | A minimal, open-source, self-hostable relay OpenPal operates a default instance of. HTTPS/WebSocket store-and-forward, deletes ciphertext on ack. Settings → Advanced → Sync exposes a **Server** field (default: OpenPal's instance; any self-hosted URL accepted) — never a forced single point of trust. |
| **Identity model** | No email, no password, no account system — matches Anytype's model, not Standard Notes'/Bitwarden's, because OpenPal has no user-account infrastructure anywhere today and building one would be a bigger philosophical departure than sync itself. A device that creates a sync group generates a recovery-phrase-style identifier (reusing `recovery.rs:35-39`'s generator) that both names the sync group on the relay (via an HKDF-derived opaque group ID, never the phrase itself) and can restore access if all devices are lost. |
| **Key model** | Each device keeps its existing local DEK, untouched. A new 32-byte random **SyncKey** is minted once per sync group and sealed per-device into a new `<data>/keys/sync.wrap`, using the exact AES-256-GCM sealing `wrap.rs` already implements for the DEK. The SyncKey never touches `app.sqlite`; it only ever encrypts sync payloads before they reach the relay. |
| **Pairing (device 2+)** | Ephemeral-keypair exchange with human-verified fingerprint confirmation, modeled on Bitwarden's "log in with device" and Signal's QR device-linking — an upgrade over the original B proposal's static shared-code design. The new device generates an ephemeral X25519 keypair and posts a join request (public key + request ID) through the relay under the sync group's opaque ID; an already-paired device shows "New device wants to join: confirm ABCD-1234?" and the user visually compares a short fingerprint before approving; only on confirmation does the approving device encrypt the SyncKey to the new device's ephemeral public key and post it back. The relay never sees the SyncKey or any code capable of deriving it — only ciphertext and public keys in transit. This is genuinely new crypto surface (unlike every other primitive in this plan, which reuses `wrap.rs`/`kdf.rs`/`recovery.rs` verbatim) and needs its own security review before shipping — see "Security / threat model." |
| **Sync unit** | Per-row change events, not a whole DB or whole-conversation blob: `{table, row_id, updated_at, op: upsert\|tombstone, ciphertext}`, one envelope per mutation, sealed under the SyncKey (same AES-256-GCM primitive as `wrap.rs`). |
| **Ordering / conflicts** | Messages are immutable once written → append-merge by Hybrid Logical Clock (HLC) across devices, no CRDT needed. Mutable fields (`conversations.title`/`project_id`, whole rows in `settings`/`custom_models`) use last-writer-wins by HLC; the losing write is kept as a `conflict-*.json` side artifact, never silently dropped — this mirrors Standard Notes' own policy of keeping both differing copies rather than auto-merging, and Joplin's "Conflicts" notebook; two independent real-world precedents converged on the same "never silently discard" answer this plan already had. Deletes are tombstones, never physical row removal. |
| **Artifacts** | Full content inlined in each envelope, keyed the same way the `artifacts` table already is (`conversation_id`, `id`) — not a separate content-addressed blob store (the relay has no fetch-by-hash endpoint), just a sender-side dedup-by-hash check before re-transferring identical content. Triggered by a periodic reconciliation sweep, not write-time capture — the only artifact write path (the agent-mode tool) has no access to sync state. A fixed 1 MiB size cap, not user-configurable, keeps larger artifacts local-only. |
| **Out of sync scope** | `plugin_records` — installed plugins, their venvs, and sidecar binaries are a per-machine fact and never sync. |
| **Relay implementation** | A single Rust binary — `axum` for HTTP/WebSocket, `SQLite` (via `rusqlite`/`sqlx`) for mailbox storage. Deliberately lighter than Standard Notes' multi-microservice-behind-Redis stack: OpenPal's relay only ever stores and forwards opaque envelopes, it doesn't do auth, billing, search, or any plaintext-dependent feature, so it doesn't need that stack's complexity. A shared `crates/sync-protocol` crate (new) defines the wire envelope/schema as plain serde types, used by both the client (`crates/core`) and the relay, so the two can never drift silently out of sync with each other. |
| **Self-hosting** | The relay ships as open source with a Docker Compose file and a single-binary + systemd deploy guide (`docs/RELAY_SELF_HOSTING.md`), mirroring Standard Notes'/Anytype's self-hosting docs. A client can point at a self-hosted relay from day one — self-hosting is not a "later" feature bolted onto a vendor-only v1. |
| **Default state** | Off. Lives entirely behind **Settings → Advanced → Sync**, gated the same way `PluginManager`'s enable/setup flow gates plugin state in `plugin_records` — zero background activity, zero new UI, zero cost for a beginner who never opens it. |
| **Mobile** | Fully deferred. No code ships until Android/iOS Tauri targets and a `keyring` backend for them exist — see "Per-platform prerequisites" below. |

### What syncs, table by table

`app.sqlite` (`openpal_core::app_db`) holds six tables (ARCHITECTURE.md "Data
on disk"); this is the per-table treatment locked above (unchanged from the
C-based version of this document — the sync-unit design didn't need to
change, only the transport):

| Table | Syncs? | Treatment |
|---|---|---|
| `conversations` | Yes | Messages (the `data` JSON blob's turns) are immutable once written → HLC-ordered append merge, no conflicts possible. `title`/`project_id` are mutable → LWW-by-HLC, loser stashed as `conflict-*.json`. |
| `artifacts` | Yes | Keyed `(conversation_id, id)` same as today; a periodic sweep dedups by content hash before re-sending — unchanged content never re-transfers (Phase 4, `crate::sync::artifact_sweep`). |
| `projects` | Yes | Small, low-frequency, whole-row LWW-by-HLC. |
| `settings` | Yes | Whole-row LWW-by-HLC. The relay server URL itself lives in this table, so it is excluded from its own payload to avoid a device silently redirecting another device's relay. |
| `custom_models` | Yes | Whole-row LWW-by-HLC — a custom model reference (HF repo/local path), not the weights themselves; a device without the matching local file simply shows it as not-yet-downloaded. |
| `plugin_records` | **No** | Installed plugins, venvs, and sidecar binaries are a per-machine fact; never leaves the device. |

## Architecture overview

```
Device A (macOS/Windows/Linux)                Device B (macOS/Windows/Linux)
┌──────────────────────────┐                  ┌──────────────────────────┐
│ app.sqlite (SQLCipher)    │                  │ app.sqlite (SQLCipher)    │
│ keyed by local DEK        │                  │ keyed by local DEK        │
│ (unchanged, ARCHITECTURE  │                  │ (unchanged)                │
│  .md "Data on disk")      │                  │                            │
└─────────────┬────────────┘                  └─────────────┬────────────┘
              │ crates/core/src/sync/ (new)                  │
              │ read local changes → HLC-stamp →              │
              │ seal under SyncKey (AES-256-GCM,               │
              │ same primitive as wrap.rs)                     │
              ▼                                                ▼
      push envelope (WS/HTTPS)                     push envelope (WS/HTTPS)
              │                                                │
              └──────────────┬─────────────────────────────────┘
                              ▼
                    OpenPal relay (or self-hosted)
              new Rust binary, axum + SQLite mailbox store
              stores/forwards ciphertext only; deletes on ack
              never decrypts, never merges, never sees SyncKey
                              │
              ┌───────────────┴────────────────────────────────┐
              ▼                                                 ▼
   Device A pulls B's envelopes                     Device B pulls A's envelopes
   unseal under SyncKey → HLC-merge → decrypt/      unseal under SyncKey → HLC-merge →
   re-encrypt under local DEK → app.sqlite          decrypt/re-encrypt under local DEK
                                                     → app.sqlite
```

Everything left of the relay is Rust in `crates/core`/`crates/app` — the
webview (`ui/`) has zero network access by CSP policy and stays that way;
this plan adds no exception, the same boundary `reqwest`-based model
downloads and `tauri-plugin-updater`'s self-update checks already respect.
`crates/core/src/sync/`, `crates/sync-protocol`, and the new relay crate
(tentatively `crates/relay` — see "Open questions" on repo placement) are
new; nothing in the existing engine, catalog, or plugin subsystems changes.

## Phased implementation plan

| Phase | Scope | Targets | Effort |
|---|---|---|---|
| 0 | Zero-network precursor: prove the per-conversation sync unit and give users a manual fallback, before any transport exists. | New `import_data` command (`crates/app/src/commands.rs`, paired with existing `export_data` at `commands.rs:3761`) + a matching reader in a new `crates/core/src/import.rs` (sibling of `export.rs`). Upserts `conversations`/`projects`/`artifacts`/`settings`/`custom_models` from an exported zip. Settings → Advanced gets an "Import from another device" action. | S |
| 1 | Relay skeleton, no crypto yet. | New `crates/relay` binary (axum + SQLite mailbox store: push/pull/ack over WS, opaque mailbox IDs, deletes on ack). New `crates/sync-protocol` crate defining the wire envelope as shared serde types. Local dev deploy + a minimal Docker Compose file. Prove the plumbing with synthetic ciphertext before any real crypto is wired up. | S/M |
| 2 | Crypto scaffolding: SyncKey, sync-group identity, pairing. | New `crates/core/src/crypto/sync_key.rs` (32-byte random SyncKey, no `Clone`, redacted `Debug`/`Display` — mirrors `key.rs:19,40-50`'s discipline). Extend `wrap.rs`'s seal/unseal functions to parametrize the wrap-file path so they cover `<data>/keys/sync.wrap` as well as `keychain.wrap` (`wrap.rs:107-158,216-244`). Reuse `recovery.rs:35-39`'s recovery-phrase generator for sync-group identity. New ephemeral-keypair pairing flow (ECDH, e.g. `x25519-dalek`) with a fingerprint-confirmation UI. New commands `sync_group_create`, `sync_pair_request`, `sync_pair_confirm` in `commands.rs`. This is the phase that most needs external security review before shipping (new crypto surface, not a reuse of existing primitives). | M/L |
| 3 | Per-row change capture + relay round trip, desktop-only. | Settings → Advanced → Sync toggle + Server field (`settings` table / `crates/core/src/settings.rs`). New `crates/core/src/sync/writer.rs`/`reader.rs`: hook `app_db`'s write paths to emit change envelopes, push/pull via the relay, HLC-ordered merge, LWW-by-HLC + `conflict-*.json` side-artifact writer for mutable-field collisions, tombstone application. | L |
| 4 | Artifact sync — **done**. | A periodic reconciliation sweep (`crate::sync::artifact_sweep`, triggered from `crates/app/src/sync_loop.rs`) dedups by content hash before pushing; a fixed internal size cap (1 MiB, no Settings UI) keeps larger artifacts local-only; the relay's previously-implicit 2 MiB body limit was replaced with an explicit, sized one. | S (smaller than originally estimated — Phase 3 had already built the encryption/outbox/LWW machinery this phase just needed to trigger) |
| 5 | Self-hosting packaging — **done, narrowed scope**. | Deploying OpenPal's own default hosted instance is deliberately deferred (still gated on the ops decisions below); this phase shipped everything that doesn't depend on that: a `/health` endpoint, per-source rate limiting on the relay, a systemd unit, and a full deploy guide — see `docs/RELAY_SELF_HOSTING.md`. Key/device rotation and removal UI was already done in Phases 2-3. | S (narrowed from the original M/L once the hosted-instance half was descoped) |
| 6 | Mobile bring-up (external-blocked, not sync-specific). | Tauri Android/iOS target scaffolding, a `keyring`-equivalent backend for each OS's secure storage, background-sync story (see "Per-platform prerequisites"). This is the phase B was chosen to fit well ahead of time. | L, external-blocked |

Phases 0–1 ship value and de-risk the plumbing without any real crypto or
user data at stake. Phase 2 is the phase that most needs outside review —
it's the one genuinely new cryptographic protocol in this plan. Phase 5's
self-hosting half shipped without needing the ops questions below answered
(a self-hoster owns those decisions for their own instance) — OpenPal
operating its own default hosted instance is the part still gated on real
answers to them, not just code.

## Security / threat model

**Confidentiality.** All sync payloads are AES-256-GCM-sealed under the
SyncKey before they ever reach the relay — the same authenticated primitive
`wrap.rs` already uses for the DEK. The relay (OpenPal's or a self-hosted
one) sees only ciphertext, HKDF-derived opaque mailbox/group IDs, and
envelope sizes/timing — never plaintext conversations, titles, or the
DEK/SyncKey. That metadata leak (roughly how much/how often a user chats)
is the same tradeoff Signal and Obsidian Sync both accept and is not hidden
without much larger engineering. Local at-rest encryption of `app.sqlite`
under the DEK is completely untouched — `DataKey`'s no-`Clone`/redacted-`Debug`
design (`key.rs:19,40-50`) still holds, because the DEK is never wrapped,
exported, or exposed to the sync path in any form.

**Integrity / availability — now a strength, not a gap.** This is the axis
that flips versus the superseded C design: a relay OpenPal (or a self-host
operator) actually runs can guarantee delivery-on-ack and detect a dropped
envelope, instead of depending on a third-party cloud provider's own file
-sync semantics and conflict-renaming behavior. A compromised or malicious
relay can still drop, delay, replay, or reorder envelopes, or correlate
mailbox activity by timing — but it cannot read or forge content (GCM
authentication), and — critically — a *self-hosted* relay means the user
who cares about this doesn't have to trust OpenPal's instance at all.

**Pairing-protocol risk — the one genuinely new attack surface.** Unlike
every other primitive in this plan (which reuses `wrap.rs`/`kdf.rs`/
`recovery.rs` verbatim), the ephemeral-keypair pairing exchange is new code
and needs its own review: a relay that's actively malicious (not just
passively observing) could attempt to inject a fake join request or
substitute its own public key for the new device's (a MITM on pairing) —
this is exactly what the human fingerprint-confirmation step exists to
catch, the same way Signal's safety-number comparison does. The confirming
device's UI must make skipping that comparison hard, not just possible —
"blindly click confirm" is a documented real-world failure mode for this
class of UX (users click through prompts under fatigue); this is a UX
security requirement, not just a protocol one.

**Stolen/compromised device.** Identical to today's threat model for the
local DEK (app.sqlite plaintext is only as safe as the device's own
lock/keychain — unaffected by any of this). A stolen device additionally
exposes its copy of the SyncKey (in `sync.wrap`, sealed under that device's
own local KEK exactly like `keychain.wrap` is today) — i.e., everything ever
synced in that sync group, not just that device's own data. This is
revocable (see below) and is a strict superset, never a regression, of the
device's existing single-device exposure.

**Revocation.** Removing a device = rotating the SyncKey and re-pairing
every surviving device (each re-confirmed via the fingerprint flow). There
is no remote-wipe of a removed device's local `app.sqlite` (out of scope —
same as today), only a guarantee that it stops being able to decrypt
*future* sync traffic. Past ciphertext it already downloaded and decrypted
is, definitionally, already in its possession — document this plainly in
the UI copy ("removing a device stops it from syncing further; data it
already has stays on it").

**Legal/subpoena exposure.** The relay holds only ciphertext plus coarse
metadata (opaque group/mailbox IDs, envelope sizes, timestamps) — a legal
request against OpenPal's hosted instance yields nothing readable, similar
to Signal's stated minimal-metadata subpoena responses. OpenPal still needs
a published retention policy (see "Ops / non-engineering considerations") —
"we can't read it" doesn't remove the obligation to say how long ciphertext
is retained and what metadata is logged.

## Ops / non-engineering considerations

Running a relay — even a blind one — is a different kind of commitment than
anything else in this codebase today, and this document should not pretend
otherwise. These need real answers, not just code, before OpenPal stands up
its own default hosted instance (self-hosting packaging, the rest of Phase
5, does not depend on any of these — see `docs/RELAY_SELF_HOSTING.md`):

- **Hosting and domain.** Where does OpenPal's default instance run, who
  pays for it, and who is paged if it's down? A relay outage degrades sync,
  it doesn't break the app (local-first, offline-capable by design) — but
  someone still has to own it.
- **Retention policy.** How long does undelivered ciphertext sit in a
  mailbox before it's purged? What's logged (timestamps, IPs, envelope
  sizes) and for how long? This needs to be published, not just decided
  internally — Standard Notes and Signal both publish this explicitly.
- **Abuse handling.** A blind relay can still be used to store/forward
  abuse-reported content it can't inspect (it's ciphertext) — the mitigation
  is account/mailbox-level rate limiting and takedown-by-group-ID, not
  content moderation. Needs a documented process before Phase 5, not
  during an incident.
- **Privacy policy.** Operating any server that touches user data (even
  ciphertext-only) is a different privacy posture than a fully local app.
  This needs an update to whatever privacy documentation OpenPal already
  publishes, reviewed before the default instance goes live.
- **Who maintains the relay long-term.** A single-maintainer local-first
  project taking on a permanent service is the single biggest philosophical
  departure in this plan — worth being explicit that this is a deliberate,
  named tradeoff (made in exchange for mobile-ready sync), not something
  that crept in unnoticed.

## Per-platform prerequisites

### Desktop (macOS, Windows, Linux) — buildable today

All three desktop OSes already have a working keychain-backed KEK
(`keychain.rs:19-20,37-69`, service `"OpenPal"`) and unrestricted network
access from the Rust layer (`reqwest` is already a workspace dependency), so
Phases 0–5 need no new per-OS branch beyond what `reqwest`/`tokio-tungstenite`
-style WebSocket clients already provide cross-platform.

### Android / iOS — do not exist yet

Mirroring `ARCHITECTURE.md`'s existing "Adding a platform (Windows/Linux)"
section, which has no mobile equivalent today, here is what "Adding a
platform: Android/iOS" would actually need to cover before Phase 6 can
start, verified against the current repo (not aspirational):

1. **Tauri mobile target scaffolding.** No `gen/android` directory exists
   anywhere in the repo. The `#[cfg_attr(mobile, ...)]` attribute in
   `lib.rs:16` is inert scaffolding that has never been wired up. This is a
   real bring-up project (Android Studio/Gradle toolchain, iOS
   Xcode/provisioning), not a config flag.
2. **Secure key storage backend.** `keyring`'s enabled Cargo features
   include no Android backend today — `sync.wrap` (and, for that matter,
   the existing `keychain.wrap`/`recovery.wrap`) cannot be sealed on
   Android until that's added. iOS's native Keychain is closer to the
   existing desktop story but still needs its own integration and testing.
3. **Background execution constraints — this is precisely why B was chosen
   over C.** Mobile OSes suspend background network/file access
   aggressively. A relay-based push/pull model degrades gracefully to
   "sync on foreground + opportunistic background fetch" using each OS's
   native background-refresh APIs; a folder-poll model (C) has no
   equivalent story at all without a file-watcher that mobile OSes won't
   let run persistently. True real-time push (APNs/FCM) is a further
   enhancement, not a Phase 6 requirement — foreground + background-refresh
   sync is enough for v1 mobile.
4. **Mobile CI.** No mobile CI runner exists in this repo today (unlike the
   desktop matrix `PORTABLE.md` documents). Needed before any mobile
   release, not before development starts.

None of Phases 0–5 are blocked on this list — they are all pure
desktop-and-server work. Phase 6 is entirely blocked on it, and none of the
four items above are sync-specific: they are the same bring-up cost any
future mobile feature would pay.

## Gotchas

- **The pairing UX must resist blind-approval.** A "confirm new device?"
  prompt that users click through without checking the fingerprint defeats
  the entire point of the human-verification step — this is a known failure
  mode for this UX pattern industry-wide, not a hypothetical.
- **Two keys, two disciplines — do not conflate them in code.** `app.sqlite`
  stays keyed by the local DEK exactly as today; sync envelopes are keyed
  by the SyncKey. A payload sealed under one must never be handed to code
  expecting the other — worth a type-level guard (a distinct newtype, not
  just a doc comment) given `DataKey`'s existing no-`Clone` precedent
  (`key.rs:19,40-50`) for exactly this class of mistake.
- **`plugin_records` exclusion will surprise some users.** "Sync my data"
  will read as "sync everything" to a beginner even though this feature is
  Advanced-gated; the UI should say explicitly that installed plugins don't
  travel, rather than let a user discover it by absence.
- **Zero cost until opted in, for real.** No background connection, poll
  loop, or relay traffic may exist unless a user has explicitly enabled
  sync in Settings → Advanced — same bar `PluginManager`'s disabled-by-default
  services are held to. A stray connection that opens on app boot "just in
  case" is exactly the always-on background complexity CLAUDE.md's
  mono-model philosophy warns against, even though this feature isn't about
  models.
- **The webview boundary is not optional.** `ui/`'s CSP allows only the
  Tauri IPC origin — every piece of this design (relay client, crypto,
  merge logic) belongs in `crates/core`/`crates/app`, the same boundary
  `reqwest`-based model downloads and `tauri-plugin-updater`'s self-update
  checks already respect. Nothing here is a reason to open a new exception.
- **A self-hosted relay pointing at the wrong URL is a silent data-leak
  risk, not just a connectivity error.** If the `settings` table's Server
  field can be changed without re-confirming pairing, a compromised device
  could redirect sync traffic to an attacker-controlled relay. Changing the
  server URL should require the same fingerprint-confirmation friction as
  pairing a new device, not be a plain text field a malicious process could
  silently edit.

## Open questions

- **Repo placement for the relay and protocol crates.** Same repo
  (`crates/relay`, `crates/sync-protocol`) for atomic versioning with the
  client, or a separate repo for independent deploy cadence and to keep
  the desktop app's build graph free of server-only dependencies? Decide
  before Phase 1.
- **Envelope schema versioning.** Needs a forward-compat story (a version
  field per envelope) settled before Phase 1 ships anything a real relay
  could end up holding — retrofitting a format after the fact is much more
  expensive than designing it in from the start.
- **Revocation UX.** Rotating the SyncKey requires every *surviving* device
  to be online and re-confirm pairing. What's the minimum-friction flow
  when a removed device was the user's primary machine, or a surviving
  device stays offline for a long stretch?
- ~~Does Phase 4's artifact size cap need to be user-configurable~~ —
  **resolved**: fixed, non-user-configurable constant (`crate::sync::
  artifact_sweep::ARTIFACT_SYNC_MAX_BYTES`, 1 MiB), no Settings UI. No
  evidence yet that this needs to be tunable, and a beginner has no
  principled way to pick a better number than a sensible default.
- **Is A's real-PAKE same-room quick-pair worth building** as a UX layer on
  top of B's relay-mediated pairing once the core sync path is proven, or
  does the fingerprint-confirmation flow cover pairing well enough that a
  same-room shortcut isn't worth the extra code path?
- **Should C's folder-transport design be kept "documented but unbuilt"
  indefinitely, or built later as an explicit no-infrastructure opt-out**
  for users who don't want to trust even a self-hosted relay's uptime?
  This document's git history preserves the full C design if that need
  materializes.
