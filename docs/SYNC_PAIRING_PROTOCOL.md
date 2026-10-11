# Sync pairing protocol — SyncKey, sync-group identity, device pairing, revocation

This document is the hardened Phase 2 protocol spec for
[`MULTI_DEVICE_SYNC.md`](design/MULTI_DEVICE_SYNC.md)'s locked decision "Pairing
(device 2+)": the ephemeral-keypair, human-confirmed device-pairing exchange
that document flagged as "genuinely new crypto surface... needs its own
security review before shipping." That review happened — five independent
adversarial passes (malicious relay, network attacker without relay access,
UX/social-engineering, revocation/rotation, replay/race/DoS) against an
initial draft — and this document is the protocol that came out the other
side, plus the findings that shaped it. Read `MULTI_DEVICE_SYNC.md` first for
the transport, sync-unit, and conflict-resolution design this protocol sits
underneath; it is unaffected by anything here.

**This document supersedes `MULTI_DEVICE_SYNC.md`'s single-`group_id`
simplification for anything pairing- or rotation-related.** That document
described one opaque mailbox ID per sync group; this protocol splits that
into two independently-derived mailbox namespaces (`control_mailbox_id`,
`data_mailbox_id_n` — see "Key derivations" below) for reasons the red-team
section explains. Phase 3 implementers should read `data_mailbox_id_n`
wherever `MULTI_DEVICE_SYNC.md` says `group_id`.

## Problem statement

The relay (`crates/relay`, Phase 1, already built) is a dumb, blind,
unauthenticated store-and-forward mailbox: `POST /mailbox/:id` pushes an
opaque envelope, `GET /mailbox/:id` returns everything pending, `DELETE
/mailbox/:id/:envelope_id` acks one. It does zero authentication or access
control — anyone who knows a `mailbox_id` can push, pull, or flood that
mailbox, by design (that's what "blind" buys: the relay operator, including
OpenPal's own default instance, never has anything worth subpoenaing).

Device pairing has to bootstrap trust between two devices that have never
talked before, over exactly that untrusted channel, ending with both devices
holding the same 32-byte SyncKey and nothing else able to derive or forge
it. That is the one genuinely new cryptographic protocol in the whole
multi-device sync design — every other primitive in this codebase
(`wrap.rs`, `kdf.rs`, `recovery.rs`) is reused verbatim from the existing
single-device key model. This document specifies that protocol, hardened
against every real finding the five reviews raised, and the local device
identity / roster / revocation machinery it turned out to need to make
"remove a device" actually mean something.

## Primitives this protocol reuses unchanged

- `DataKey` (`crates/core/src/crypto/key.rs:19-29`) — 32-byte `OsRng` key,
  no `Clone`, hand-written redacted `Debug`/`Display`. SyncKey is
  constructed exactly this way.
- `wrap.rs` (`:107-158,216-244`) — AES-256-GCM seal/unseal of a key under a
  KEK, to a wrap file, with write-verify-rename-fsync durability and a
  `.bak` generation. Extended (Phase 2, already planned) to take a
  caller-supplied path so it covers `sync.wrap`, `sync_group.wrap`, and
  `sync_identity.wrap` in addition to `keychain.wrap`.
- `kdf.rs`'s `derive_subkey` (`:63-`) — HKDF-SHA256 key separation with an
  `info` label. Every derived key/mailbox-ID in this protocol uses this
  same construction; no new HKDF implementation is introduced.
- `recovery.rs:35-39` (`generate`) — 128 bits of `OsRng`, Crockford Base32,
  hyphen-grouped. Reused verbatim for `group_secret`. Its `encode`/`group`
  helpers (`:59-` region) are reused, not reimplemented, for the shorter
  pairing fingerprint below.

## Threat model

The relay is assumed **actively malicious or network-attacker-controlled**,
not merely a passive observer: it (or anything on-path) can read every
message, forge new ones, reorder, delay, drop, or replay anything, and knows
every `mailbox_id` it routes on (routing requires that). It never sees
`group_secret`, any device's private key material, or SyncKey plaintext.
The user's out-of-band channel (reading/scanning `group_secret` from one
device's screen to another) is trusted — nothing in any of the five reviews
attacked that channel, and this protocol doesn't try to remove the need for
it.

## Key derivations

All HKDF below is HKDF-SHA256 via `kdf::derive_subkey`'s construction (no
salt — every input here is already uniformly random, never a passphrase).
`||` is byte concatenation; `gen_n` is a `u32`, big-endian, appended to the
`info` string's bytes.

| Name | Formula | Purpose |
|---|---|---|
| `control_mailbox_id` | `HKDF(group_secret, info="openpal-sync-control-mailbox-v1")`, hex | Fixed forever. Carries `join_request`/`join_challenge`/`join_confirm` and `rotate` envelopes. Must be derivable by any device that knows only `group_secret` — including a brand-new joiner and a device replaying a rotation delivery to an offline survivor. |
| `mailbox_auth` | `HKDF(group_secret, info="openpal-sync-mailbox-auth-v1")` | HMAC-SHA256 key. Every pairing/rotation envelope is MAC'd under this key before the relay will ever cause it to be *displayed* to a user — see "Fix: mailbox authentication" below. |
| `data_mailbox_id_n` | `HKDF(group_secret, info="openpal-sync-data-mailbox-v1" \|\| gen_n)` | Phase 3 conversation-data mailbox, **generation-scoped**. Rotates on every device removal (see "Revocation"). |
| `pairing_key` | `HKDF(ECDH(a_eph_priv, b_eph_pub) \|\| group_secret, info="openpal-pairing-v1" \|\| control_mailbox_id \|\| request_id)` | AEAD key for `encrypted_synckey` in `join_confirm`. Binds `group_secret` into the *key*, not just the mailbox address — see red-team finding malicious-relay/A. |
| `fingerprint` | `truncate(SHA-256(a_eph_pub \|\| b_eph_pub \|\| control_mailbox_id), 50 bits)`, Crockford-Base32-encoded via `recovery.rs`'s `encode`/`group` helpers, 2 groups of 5 | Human-comparable transcript binding — see red-team finding ux/A. |
| `rotation_key` | `HKDF(ECDH(initiator_box_priv, survivor_box_pub), info="openpal-sync-rotation-v1" \|\| control_mailbox_id \|\| target_device_id \|\| gen_n)` | Per-survivor AEAD key for delivering a rotated SyncKey. Static-static ECDH (both box keys are persistent, not ephemeral) — secure against anyone who doesn't hold one of the two private halves, independent of `group_secret`. |

Every device that has ever joined the group holds three long-term secrets,
each sealed under that device's own local KEK via `wrap.rs`, one secret per
wrap file:

| File | Contents | Minted |
|---|---|---|
| `<data>/keys/sync_group.wrap` | `group_secret` | Group creation (Device A) or on completing pairing (every later device) |
| `<data>/keys/sync.wrap` | Current-generation `SyncKey` | Same, rewritten in place on every rotation |
| `<data>/keys/sync_identity.wrap` | Ed25519 signing keypair (`identity_sign_priv/pub`) **and** X25519 static keypair (`device_box_priv/pub`) | Same — minted once, never rotated (device removal invalidates trust in it by dropping it from the roster, not by changing it) |

The **signing** key authenticates control-plane messages this device sends
(roster updates, rotation). The **box** key is a separate persistent X25519
keypair used only for static-static ECDH when this device is the *target*
of a rotation delivery. Splitting them (rather than one key doing both
jobs, à la XEdDSA) mirrors the "one wrap file, one secret" convention this
crate already uses for `keychain.wrap` vs. `recovery.wrap`, and keeps the
signing key's only job being "prove this message came from me," never key
agreement.

## Group creation (Device A, first device — no pairing)

1. Generate `group_secret`: `recovery.rs:35-39`'s generator, verbatim.
2. Generate this device's persistent identity: Ed25519 signing keypair +
   X25519 box keypair. Seal into `sync_identity.wrap`.
3. Generate `SyncKey`: `DataKey::random()`'s construction. Seal into
   `sync.wrap`.
4. Seal `group_secret` into `sync_group.wrap`.
5. Initialize the local roster (new `sync_devices` table in `app.sqlite`,
   covered by the existing SQLCipher/DEK encryption — no new wrap file
   needed, this is local metadata, not shared secret material) with one
   row: this device, `gen_n = 0`.
6. The relay learns nothing at this step. Device A doesn't touch it until
   "Add a device" opens or Phase 3 data sync begins.

## Pairing a second device

Device A's "Add a device" screen must be explicitly open to receive/confirm
a join — not an always-on listener, per `MULTI_DEVICE_SYNC.md`'s "zero cost
until opted in" rule. Each open of that screen is **one pairing session**,
tracked by a session token generated at open time; a `join_request` arriving
before the button press, or a second concurrent one, is dropped, never
queued or displayed (closes ux/B, replay-race-dos/Attack-1, and
network-attacker/A — see red-team section).

1. **Out-of-band**: user reads/scans `group_secret` from Device A's screen
   into Device B. `group_secret` never transits the relay.

2. **Device B — `sync_pair_request`.** Derives `control_mailbox_id` and
   `mailbox_auth` locally from `group_secret`. Generates a fresh ephemeral
   X25519 keypair (`b_eph_priv`, `b_eph_pub`, discarded after use — no
   ratchet, single-message delivery) and, if it doesn't already have one
   from a prior aborted pairing, a persistent identity (signing + box
   keypair) — sealed immediately, before any network I/O, so a
   crash/kill mid-pairing never leaves key material only in memory.
   Generates `request_id`: 16 bytes, `OsRng` (explicit CSPRNG, not just
   "random" — closes replay-race-dos/Fix-4). Prompts for a short device
   label (e.g. "Sarah's MacBook") the user types. POSTs:

   ```
   JoinRequest {
     type: "join_request", control_mailbox_id, request_id,
     b_eph_pub, b_identity_pub, b_box_pub, b_device_label,
     requested_at_ms,
     mac: HMAC(mailbox_auth, canonical(all fields above)),
   }
   ```

   with `ttl_seconds: 600` on the relay push (see "Relay hardening"). Waits
   up to 5 minutes (client-side), re-POSTing the identical envelope (same
   `request_id`, refreshed `requested_at_ms`) every ~15s so a
   selectively-deleted request becomes detectable rather than silently
   gone (closes network-attacker/B).

3. **Device A — verify, then challenge.** Polls `control_mailbox_id`.
   For each envelope: recompute the MAC under locally-derived
   `mailbox_auth` *before touching anything else*. **A `join_request` that
   doesn't carry a valid MAC is discarded silently — never rendered, never
   shown to the user, indistinguishable from noise.** This is the
   structural fix, not the fingerprint: only someone who knows
   `group_secret` can produce a request Device A will even acknowledge
   exists (closes malicious-relay/B and the entire premise of
   ux/A — see red-team section for why the fingerprint-widening fix alone
   would not have been enough).

   Only if a request that arrived after the current session's button-press
   MACs correctly, and no other request is already pending this session,
   does A act: generates a fresh ephemeral X25519 keypair (`a_eph_priv`,
   `a_eph_pub`), computes `fingerprint` from `(a_eph_pub, b_eph_pub,
   control_mailbox_id)`, and POSTs:

   ```
   JoinChallenge {
     type: "join_challenge", control_mailbox_id, request_id, a_eph_pub,
     challenged_at_ms,
     mac: HMAC(mailbox_auth, canonical(...)),
   }
   ```

   Renders exactly one immutable card: `b_device_label`, the fingerprint,
   Confirm/Reject — bound to this literal `request_id` at render time. A
   later poll can never mutate what's on screen out from under an unmoved
   cursor (closes replay-race-dos/Attack-1).

4. **Device B — mutual fingerprint.** Polls for a `join_challenge`
   matching its `request_id`; MAC-verifies it the same way. Computes the
   identical `fingerprint` from `(a_eph_pub, b_eph_pub, control_mailbox_id)`
   and displays it. The user now compares two independently-computed,
   MAC-gated, transcript-bound values — the actual security boundary, same
   role as Signal's safety-number comparison, but unlike the original
   draft this one cannot be reached by anyone who doesn't know
   `group_secret` in the first place.

5. **Device A — explicit confirm.** On the user tapping Confirm (never
   auto-approve): computes `shared = ECDH(a_eph_priv, b_eph_pub)`,
   `pairing_key = HKDF(shared || group_secret, info=... || request_id)`,
   AEAD-encrypts `SyncKey` under `pairing_key` with associated data
   `(type="join_confirm", control_mailbox_id, request_id)` (closes
   malicious-relay/Fix-2 — a ciphertext can't be replayed into a different
   pairing event), signs `(control_mailbox_id, request_id, a_eph_pub,
   current gen_n)` with `identity_sign_priv`, and POSTs:

   ```
   JoinConfirm {
     type: "join_confirm", control_mailbox_id, request_id,
     a_identity_pub, a_box_pub, gen_n, encrypted_synckey,
     a_sig: Ed25519(a_identity_sign_priv, "..."),
     mac: HMAC(mailbox_auth, canonical(...)),
   }
   ```

   Also posts a signed `DeviceAdded{new_device_id, b_identity_pub,
   b_box_pub, b_device_label, gen_n}` broadcast so any *other* already-online
   survivor updates its roster too — Device A is the only device that
   directly witnessed this pairing, so it's the one that has to vouch for
   it group-wide. Deletes the `join_request`/`join_challenge` envelopes
   from the relay immediately.

6. **Device B — explicit confirm, then complete.** MAC-verifies
   `join_confirm`, computes `shared = ECDH(b_eph_priv, a_eph_pub)`, derives
   `pairing_key` identically, AEAD-decrypts `SyncKey` (AEAD failure here
   means a forged/tampered envelope slipped past the MAC — treated as a
   hard pairing failure, never a silent retry). Only *then*, with a
   decrypted-but-not-yet-installed `SyncKey` in hand, does B ask its own
   user for an explicit "codes match, finish pairing?" tap — implicit
   trust-on-successful-decrypt is not enough (closes malicious-relay/Fix-3:
   this catches an implementation bug in the MAC/AEAD chain that a
   silent auto-accept would hide). On confirmation: seals `SyncKey` into
   `sync.wrap`, seals `group_secret` into `sync_group.wrap`, adds Device A
   (and, once it arrives, every roster entry A's `DeviceAdded` /
   pre-existing roster snapshot vouches for) to its local `sync_devices`
   table, discards all ephemeral key material, deletes the `join_confirm`
   envelope. Device B is now a full member of generation `gen_n` and can
   read/write `data_mailbox_id_n` (Phase 3).

## Revocation and rotation

"Remove device" is initiated by exactly one surviving device (the
initiator) against exactly one target, at a time — the UI enforces
one-in-flight removal (see "Residual risk" below for why).

1. Initiator mints `SyncKey_new` (`DataKey::random()`), bumps
   `gen_n_new = gen_n + 1` locally, removes `target_device_id` from its own
   roster.
2. For every **other** device still in its roster (i.e. every survivor,
   never the target), the initiator computes `rotation_key` (static-static
   ECDH against that survivor's cached `box_pub`, bound to `group_secret`'s
   descendant `control_mailbox_id`, `target_device_id`, and `gen_n_new`),
   AEAD-encrypts `{gen_n_new, SyncKey_new}`, and POSTs one `Rotate` envelope
   per survivor to `control_mailbox_id`:

   ```
   Rotate {
     type: "rotate", control_mailbox_id, target_device_id, gen_n_new,
     for_device_id: <this survivor's id>,
     ciphertext: AEAD(rotation_key, {gen_n_new, SyncKey_new}),
     initiator_sig: Ed25519(initiator_identity_sign_priv,
       (control_mailbox_id, target_device_id, gen_n_new, for_device_id, ciphertext)),
   }
   ```

   with a long `ttl_seconds` (90 days, the relay's max — see "Relay
   hardening") so it survives until an offline survivor next comes online,
   unlike pairing envelopes' 600s.
3. Every survivor, on polling `control_mailbox_id`, sees every `Rotate`
   copy (it isn't addressed to it specifically at the relay level — the
   relay is still blind) but only decrypts the one where `for_device_id`
   matches itself. **Before decrypting anything**, it verifies
   `initiator_sig` against `initiator_device_id`'s `identity_sign_pub` **as
   currently cached in its own local roster** — a signature from a device
   already absent from the verifier's roster (already-removed in an
   earlier generation) is rejected outright, no exceptions. On a valid
   signature and successful AEAD open: removes `target_device_id` from its
   own roster, rewrites `sync.wrap` with `SyncKey_new`, advances its local
   `gen_n`, and deletes its own copy of the envelope. It now reads/writes
   `data_mailbox_id_{gen_n_new}` — a mailbox address the removed device
   cannot derive without also knowing the new generation number, which it
   is never sent.
4. The removed device's copy of `group_secret` still lets it compute
   `control_mailbox_id` (fixed, by design — see "Key derivations") and read
   traffic there forever, but every future `Rotate`/`DeviceAdded` message
   there is either not addressed to it (`for_device_id` never matches) or,
   if it tries to forge its own, is rejected by every survivor because its
   `identity_sign_pub` is no longer in anyone's roster. It cannot compute
   any future `data_mailbox_id_n` for `n > gen_n` at time of removal —
   Phase 3 conversation sync traffic (the high-frequency, high-information
   channel) is unobservable to it going forward. This is the one
   deliberate compromise: `control_mailbox_id` itself never rotates
   (nothing else lets a brand-new joiner or an offline survivor find it
   with only `group_secret`+cached roster in hand), so a removed device can
   still observe *that* mailbox's low-frequency pairing/rotation metadata
   (timing and size of future `join_request`/`Rotate` traffic) indefinitely.
   This is documented in the UI as "removing a device stops it from syncing
   further data; it may still see *that other devices are pairing or being
   removed*, at some future point, never what was said."

### Periodic rotation (no device being removed)

A security review of this feature found that, absent any rotation cadence,
compromising **any one** of a user's own devices, at any point, exposes the
*entire* cross-device sync history under that generation's `SyncKey` — and
all future traffic until a human happens to remove a device. The mitigation
is a second, automatic trigger for the exact same mechanism above, silent
and default-path (no setting a beginner has to configure):

- **Trigger: 90 days elapsed OR 500 envelopes pushed/applied since the last
  rotation, whichever comes first**, tracked in `sync_key_state`
  (`crate::sync::key_state`). 90 days matches this protocol's existing
  rotation-TTL order of magnitude; 500 envelopes bounds a heavy multi-device
  household independently of elapsed time.
- **Initiator: the lowest `device_id` currently in the roster**
  (`pairing::is_designated_initiator`), evaluated identically and
  independently by every device each tick — no presence/heartbeat protocol
  needed. If that device happens to be offline, rotation is simply late (the
  check is "generation age/count `>=` threshold," not an exact match).
- **Message shape: identical to removal's `Rotate`, with
  `target_device_id: null`** instead of an id — every current roster member
  (including the initiator) ends up on the new key, and nobody is dropped
  from the roster. `rotation_key`'s HKDF `info` and `Rotate`'s AEAD AAD both
  bind an explicit 1-byte presence tag ahead of `target_device_id`'s bytes
  (`0x00` = none, `0x01` = present) so the two cases can never be confused
  with each other.
- Every other step — static-static ECDH per survivor, MAC + Ed25519
  signature over `control_mailbox_id`, apply order, idempotency-by-`gen_n` —
  is exactly the mechanism above, unmodified. See "Residual risk" below for
  why this is safe under the same reasoning that already covers removal.

## Relay hardening (addendum to the already-built Phase 1 relay)

Three changes to `crates/relay`, all content-blind — no ciphertext
inspection, so "the relay never inspects ciphertext" still holds:

1. **`ttl_seconds` on push.** `sync-protocol::Envelope` and
   `PushEnvelopeRequest` gain `ttl_seconds: Option<u64>`, clamped
   server-side to `[60, 30*24*3600]` (default 600 if omitted). `store.rs`
   persists `expires_at_ms`; a periodic sweep task (`main.rs`) deletes
   expired rows regardless of ack state. Needed because "5 minute expiry"
   in the original draft was purely client-side and never caused deletion —
   an attacker's flood was permanent until manually acked.
2. **Per-mailbox envelope count cap.** `store.rs::push` rejects with `429`
   once a `mailbox_id` holds ≥200 pending envelopes (generous enough for a
   plausible rotation fan-out across many survivors, tight enough to bound
   worst-case `GET` payload size and parse cost).
3. **No change to what the relay authenticates or inspects.** Both of the
   above are pure counters/timestamps the relay already stores
   (`created_at_ms` exists today); they add no new trust the relay is asked
   to extend to a `mailbox_id`'s traffic.

## Red team findings and fixes

Every finding below is real unless explicitly marked "not adopted," in
which case the reasoning for rejecting it is given — nothing is softened
into vague language in either direction.

### malicious-relay

- **Finding A — forge `join_confirm`, impersonate Device A with no real
  Device A involved.** *Fully vulnerable in the draft.* **Fixed**: the MAC
  gate (Fix: mailbox authentication, below) means an attacker can't get a
  `join_request` displayed at all without knowing `group_secret`, so there
  is no live pairing session to attach a forged `join_confirm` to in the
  first place; independently, `pairing_key` now binds `group_secret` into
  the AEAD key itself (not just the mailbox address), so even a
  MAC-gate implementation bug wouldn't let a forged `join_confirm` decrypt
  at Device B.
- **Finding B — forge `join_request`, impersonate Device B.** *Fully
  vulnerable in the draft.* **Fixed** by the same MAC gate: a `join_request`
  without a valid `mailbox_auth` HMAC is discarded before it is ever
  rendered — there is nothing for a rushed user to mis-click, because
  nothing attacker-forged reaches the UI.
- **Fix 1 (bind `group_secret` into the pairing key)** — adopted, see
  `pairing_key`'s formula above.
- **Fix 2 (bind type/mailbox/request_id into AEAD associated data)** —
  adopted, see `join_confirm`'s AEAD AAD above.
- **Fix 3 (Device B's own explicit confirmation, not implicit
  trust-on-decrypt)** — adopted, step 6 of pairing.
- **Fix 4 (Device A's session tracks the button press, doesn't passively
  surface whatever appears)** — adopted: one pairing session per "Add a
  device" open, exactly one card rendered.

### network-attacker-no-relay

- **Entropy question (`group_secret` leaking via `group_id`)** — confirmed
  sound in the draft, no finding, unchanged here.
- **Attack A — fake `join_request` race / confused fingerprint.**
  *Partially prevented in the draft (fingerprint-UI-dependent).* **Fixed**
  structurally by the MAC gate: without `group_secret` an attacker cannot
  produce a `join_request` Device A's UI will ever show, so there is no
  "wrong entry" for a rushed user to confirm — this is a stronger fix than
  the review's own proposal (HMAC every envelope), which is exactly what
  was adopted.
- **Attack B — selective envelope deletion (silent suppression).** *Fully
  vulnerable, unmentioned in the draft.* **Fixed**: Device B re-POSTs its
  `join_request` every ~15s within the 5-minute window; a request that
  appears then vanishes from Device A's poll is a detectable, not silent,
  failure. Full closure would need relay-enforced write auth, which the
  relay deliberately doesn't do (staying content/identity-blind is the
  point of Phase 1's design) — this residual is accepted, documented, not
  hidden: a sufficiently active on-path attacker can still degrade pairing
  to "keeps failing," it just can no longer do so invisibly.
- **Attack C — mailbox flood.** *Fully vulnerable, low severity.* **Fixed**
  by the relay hardening's count cap + TTL sweep above.
- **Attack D — passive eavesdrop on `join_confirm`.** *Fully prevented
  already*, no finding, unchanged.
- **Fix 3 (bind context into AEAD)** — adopted (same as malicious-relay
  Fix 2).
- **Fix 4 (>1 concurrent `join_request` is an anomaly, never a picklist)**
  — adopted: exactly one card per session, extras dropped.

### ux-social-engineering

- **Attack A — fingerprint collision grinding.** *Fully vulnerable in the
  draft* (fingerprint derived from `b_eph_pub` alone, ~20 bits, gameable
  offline). **Fixed** two ways, deliberately redundant: (1) structurally,
  the MAC gate means an attacker can't get any of its own keys into a live
  session to grind against in the first place — there is no legitimate
  transcript to collide with unless the attacker already knows
  `group_secret`, at which point it's a full group member and fingerprint
  comparison isn't the remaining defense anyway; (2) belt-and-suspenders,
  the fingerprint itself is now a 50-bit transcript hash of *both*
  ephemeral keys plus the mailbox ID, not 20 bits of one party's key alone
  — cheap, and it catches a bug in (1) rather than depending on it.
- **Attack B — decoy/duplicate `join_request` flood.** *Fully vulnerable in
  the draft.* **Fixed** by the MAC gate (attacker-forged requests never
  render) plus the one-card-per-session rule (even a MAC-valid duplicate —
  impossible without `group_secret`, but hypothetically — never becomes a
  picklist).
- **Attack C — rushed/backgrounded confirmation.** *Only partially
  addressed in the draft's UI shape.* **Not further hardened here beyond
  what the draft already had** (explicit tap, no auto-approve) — the
  review's own proposed decoy-selection UI (fix 2: "show 3 codes, 1 real +
  2 decoys, require tapping the match") is judged **not adopted**: it was
  proposed as a mitigation for attack A (a gameable fingerprint), and once
  the fingerprint is transcript-bound and MAC-gated, there is no
  attacker-controlled decoy to construct in the first place — building
  decoy-selection UI against a threat that's already closed at the
  protocol layer would be complexity with no attacker it stops. The
  `b_device_label` shown alongside the fingerprint (fix 4, adopted) is kept
  as a legitimate, independent second signal for the case that *is* still
  live — a user with multiple of their own devices pairing at once, or
  simple fat-fingering, not an attacker.

### revocation-rotation

- **Finding 1 — `group_id` never rotates, permanent metadata surveillance.**
  *Fully vulnerable in the draft.* **Fixed** for the high-value channel:
  `data_mailbox_id_n` is generation-scoped and a removed device can never
  derive a future generation's address. `control_mailbox_id` deliberately
  does **not** rotate (a brand-new joiner and an offline survivor being
  delivered a rotation must be able to find it from `group_secret` alone,
  with no other state) — the residual is documented above under
  "Revocation and rotation," not hidden: a removed device can still observe
  low-frequency pairing/rotation *timing and size*, never Phase-3 content
  or its cadence.
- **Finding 2 — spiteful flood/DoS.** *Fully vulnerable.* **Fixed** by the
  relay hardening's count cap + TTL sweep, same as network-attacker/C.
- **Finding 3 — forging the rotation message with the very key being
  rotated.** *Fully vulnerable, and the sharpest bug in the original
  draft.* **Fixed** by removing SyncKey-based authentication from rotation
  entirely: every device mints a persistent Ed25519 identity at
  pairing/creation time, and `Rotate`/`DeviceAdded` messages are signed
  with that identity key, verified against the verifier's own
  independently-maintained roster — never against the key being replaced.
  A device removed in generation *N* cannot forge a valid signature for
  generation *N+1* because its identity key is dropped from every
  survivor's roster in the same step that processes its removal.
- **Finding 4 — MITM against a "lighter-weight" re-pairing variant that
  drops the fingerprint step.** *Fully vulnerable if adopted.* **Not
  applicable to the final design**: revocation here never re-runs a
  pairing-like exchange per survivor at all — it's a signed, individually
  AEAD-encrypted broadcast over static identity/box keys established once,
  at original pairing time, which already went through full fingerprint
  confirmation. There is no "lighter-weight variant" left to accidentally
  strip the human check from.
- **`group_id` rotation question** — answered: `data_mailbox_id_n` rotates,
  `control_mailbox_id` does not, for the reasons above.

### replay-race-dos

- **Attack 1 — UI mis-binds confirm to the wrong pending `request_id`.**
  *Only partially prevented in the draft.* **Fixed**: exactly one immutable
  card per session, bound to its `request_id` at render time, never mutated
  by a later poll; confirmed/rejected envelopes are deleted from the relay
  immediately after use.
- **Attack 2 — replaying a captured `join_confirm`.** *Fully prevented
  already*, no finding change; the hygiene fix (delete envelopes after use)
  is adopted anyway, purely to bound relay storage.
- **Attack 3 — mailbox flood / resource exhaustion.** *Fully vulnerable.*
  **Fixed** by the relay hardening's count cap (fix 1) + TTL sweep (fix 2,
  now `ttl_seconds`-driven rather than a fixed relay-wide constant, so a
  600s pairing envelope and a 30-day rotation envelope can both be swept
  correctly); client-side, Device A only ever renders one card, and dropped
  extras are simply never fetched into UI state (fix 3, subsumed by the
  one-card-per-session rule); `request_id` is now an explicit 16-byte
  `OsRng` value (fix 4, adopted, see pairing step 2).

## Residual risk (documented, not silently accepted)

A narrow race exists between an initiator posting a `Rotate`/removal
broadcast and every survivor having actually processed it: during that
window, a survivor that hasn't polled yet still has the about-to-be-removed
device's identity key in its roster and would accept a signature from it.
The removed device could, in principle, race a conflicting signed message
of its own before it's cut off everywhere. Mitigation: the UI enforces one
removal in flight at a time, and roster application is idempotent
per-`(target_device_id, gen_n)` — the first validly-signed message a
survivor sees for a given `gen_n` wins, and once applied that generation
slot is closed to any later competing claim. This is judged acceptable
because device removal is a deliberate, low-frequency, human-initiated
action, and the race requires the device being removed to already be
actively adversarial (racing its own removal) rather than merely lost or
stolen — not zero risk, but not the load-bearing security property of this
protocol either. Worth revisiting if telemetry ever shows this window is
exploited in practice.

**Periodic rotation reuses this exact argument for a second trigger
source.** It is still low-frequency (at most every 90 days, or every 500
envelopes) and the same per-`(target_device_id, gen_n)` — generalized to
per-`gen_n` when `target_device_id` is absent — idempotency closes the same
race: whichever validly-signed `Rotate` a survivor sees first for a given
`gen_n` wins, and a stale or duplicate `gen_n` is rejected outright
(`key_state::set_current_gen`'s monotonic guard). The single-initiator rule
(lowest device id) is the added mechanism keeping concurrent-initiation
itself rare in the first place — the more interesting race isn't "does an
adversarial device win a fight over its own removal" (periodic rotation
removes nobody) but "do two honest devices simultaneously decide it's time
to rotate," which the initiator rule already avoids in the common case and
the idempotent apply order resolves harmlessly on the rare occasions the
roster is itself mid-change (e.g. right after a pairing completes) and two
devices briefly disagree about who's lowest.

## Implementation task list

| # | Scope | Files | Effort |
|---|---|---|---|
| 1 | SyncKey type | New `crates/core/src/crypto/sync_key.rs`: 32-byte `OsRng` key, no `Clone`, redacted `Debug`/`Display` — mirrors `key.rs:19,40-50`'s `DataKey` discipline exactly (a distinct newtype, not a type alias, so a SyncKey can never be handed to code expecting a local DEK). | S |
| 2 | Device identity type | New `crates/core/src/crypto/sync_identity.rs`: Ed25519 signing keypair (`ed25519-dalek`, new dependency) + X25519 static box keypair (`x25519-dalek`, already implied by the pairing ECDH), both `OsRng`-generated, private halves non-`Clone`/redacted like `DataKey`. Serialization helpers for the public halves (32 bytes each, fixed-size). | S |
| 3 | Wrap-file path parametrization | Extend `wrap.rs`'s `write_wrap`/`read_wrap` (already accept a `path: &Path`, confirm no further change needed beyond what Phase 2 already planned) to be exercised against `sync_group.wrap`, `sync.wrap`, `sync_identity.wrap` — three new callers, zero new crypto in this module. | S |
| 4 | Pairing protocol messages + crypto | New `crates/core/src/sync/pairing.rs`: `JoinRequest`/`JoinChallenge`/`JoinConfirm`/`Rotate`/`DeviceAdded` structs (serde), canonical-encoding for MAC/signature input (fixed field order, never JSON's insertion order), `control_mailbox_id`/`mailbox_auth`/`pairing_key`/`fingerprint`/`rotation_key` derivation functions per the "Key derivations" table, MAC verify-before-render gate, AEAD seal/open (reuse `ring::aead::AES_256_GCM` exactly as `wrap.rs` does — no new AEAD primitive). | M |
| 5 | Local roster | New `sync_devices` table in `app.sqlite` (covered by existing SQLCipher/DEK encryption, no new wrap file): `device_id`, `identity_sign_pub`, `box_pub`, `label`, `gen_added`, `is_self`. New `crates/core/src/sync/roster.rs`: read/write, "am I still a member as of the latest generation I've applied" query, signature-verify-against-roster helper used by both pairing (verify `a_identity_pub`/`a_sig`) and rotation (verify `initiator_sig`). | M |
| 6 | Tauri commands | In `crates/app/src/commands.rs`: `sync_group_create()` (group creation flow above), `sync_pair_request(group_secret) -> {request_id, fingerprint}` (Device B side; runs the 5-minute poll/re-POST loop in a background task, emits a Tauri event on `join_challenge` arrival and again on completion/failure), `sync_pair_confirm(request_id, approve: bool)` (Device A side, called once the user taps the rendered card; on `approve` performs step 5 of pairing and the `DeviceAdded` broadcast), `sync_device_list() -> Vec<RosterEntry>`, `sync_device_remove(device_id)` (revocation flow above). | M/L |
| 7 | Relay hardening | `crates/sync-protocol/src/lib.rs`: add `ttl_seconds: Option<u64>` to `Envelope`. `crates/relay/src/routes.rs`: same field on `PushEnvelopeRequest`/`StoredEnvelope`, server-side clamp `[60, 2_592_000]`, `429` on a mailbox's 200th pending envelope. `crates/relay/src/store.rs`: `expires_at_ms` column, `push` takes/stores it, new `sweep_expired()` returning rows deleted, `pending_count(mailbox_id)`. `crates/relay/src/main.rs`: periodic sweep task (e.g. every 60s). | M |
| 8 | Fingerprint-confirmation UI | New pairing screens (Device A: "New device wants to join" card — device label, fingerprint, Confirm/Reject, single-card-per-session enforced client-side by construction since the backend only ever surfaces one event stream per open session; Device B: "Confirm this matches the other screen" — fingerprint, Confirm/Reject, gated on a successful-but-not-yet-installed decrypt per pairing step 6). Both screens must make the fingerprint the visually dominant element (not a small caption under a big "Confirm" button — the documented blind-approval failure mode this whole protocol's human step exists to resist) and must not offer a path to confirm before the fingerprint has actually rendered (no default-focused Confirm button, no keyboard-shortcut confirm). | M |
| 9 | Revocation UI | "Remove device" in Settings → Advanced → Sync: device list from `sync_device_list`, confirm dialog naming the device being removed, disabled while another removal is in flight (enforces the one-at-a-time convention "Residual risk" relies on), post-removal copy stating plainly what is and isn't cut off (see "Revocation and rotation" §4's UI-copy note). | S |
| 10 | Tests | Unit: MAC-gate rejection of an unsigned/wrong-key `join_request` never reaching a "pending" state; `pairing_key`/`fingerprint`/`rotation_key` derivation vectors; roster signature-verify rejecting a message from a device not (or no longer) in the roster; relay `sweep_expired`/count-cap behavior. Integration: full pairing round trip over an in-memory relay (mirrors `routes.rs`'s existing `push_pull_ack_round_trip` test); full removal round trip across 3 simulated devices confirming the removed one can't derive `data_mailbox_id_{n+1}`. | M |

Phases 1–5 of `MULTI_DEVICE_SYNC.md` are unaffected by this document except
where noted: Phase 1's relay gets the `ttl_seconds`/count-cap addendum in
task 7; Phase 3's "per-row change capture" must read `data_mailbox_id_n`
wherever it currently plans to read `group_id`. Nothing here changes
Phase 3's HLC/LWW/tombstone sync-unit design, Phase 4's artifact sync, or
Phase 5's hosting/self-hosting story.
