# Remote Access (desktop as inference host)

Remote Access lets another device — today, anything that can speak HTTPS and
JSON; eventually, an OpenPal mobile client — pair with this desktop and use it
as the inference source: chat completions run on the desktop's already-loaded
model, proxied over the LAN (or an already-connected overlay network) to the
paired device. Off by default (Settings → Advanced → "Remote access"). This is
the one explicit, gated exception to "the engine only ever listens on
loopback" (`ARCHITECTURE.md`) — `crate::remote::gateway` binds `0.0.0.0`, but
only while this setting is on, and only ever reachable with a valid
paired-device bearer token.

This document is the protocol/implementation reference — read it before
building anything that talks to the gateway, especially a future mobile
client (see "Building the mobile client" below).

## Module map (`crates/core/src/remote/`)

- `mod.rs` — `RemoteAccessManager`: the public API (`crates/app`'s `AppState`
  holds one, next to `EngineManager`/`PluginManager`). Owns the gateway's
  listening task and orchestrates pairing sessions.
- `gateway.rs` — the inbound HTTP(S) server (`axum` + `axum-server`'s rustls
  TLS termination). Routes, auth, and the chat-completions proxy.
- `pairing.rs` — wire types (`PairingPayload`, `PairingEvent`) and the
  pairing-code generator.
- `store.rs` — `app_db`-backed persistence: `remote_access_config` (a fourth
  singleton-row store, alongside settings/custom-models/plugin-records — see
  `crate::app_db`'s `SCHEMA_V4`) and `paired_devices` (a real multi-row
  table).
- `wol.rs` — magic-packet build/send + network-interface enumeration.
- `wol_status.rs` — best-effort per-OS "is Wake-on-LAN enabled" detection.
- `discovery.rs` — mDNS advertisement (`_openpal._tcp.local.`) via `mdns-sd`.

## Why a self-contained pairing model, not the sync feature's device identity

A separate feature (async multi-device conversation sync via a relay) built
an Ed25519/X25519 device-identity + roster model for its own pairing needs
(`crates/core/src/crypto/device_identity.rs`, `crates/core/src/sync/pairing.rs`
— implemented and shipped after Remote Access). Remote Access does **not**
use it, deliberately:

- **Different threat models.** The sync feature's relay is fully untrusted
  and blind — it's assumed actively malicious. Remote Access's channel is
  either an already-TLS-pinned LAN connection or an already-encrypted overlay
  network (Tailscale/WireGuard) the user set up themselves; the gateway's own
  TLS + bearer token is defense-in-depth on top of that, not the only layer.
- **No shared code existed at the time.** Remote Access shipped before the
  sync feature's device-identity module was built. Blocking Remote Access on
  it — or building it here for the sync feature to adopt later — was
  explicitly decided against; see the git history around this feature's
  introduction for the cross-session coordination that led here.

**Migration note, not to be forgotten**: the shared device-identity module
now exists (`crates/core/src/crypto/device_identity.rs`) and is relay-agnostic
— nothing structural blocks migrating onto it. Revisit `pairing.rs`/`store.rs`
to adopt it for a unified "trusted devices" model across both features. Until
then, `paired_devices` stores an opaque per-device bearer token (salted-hash,
mirroring `applock.rs`'s `hash_pin`/`new_salt` shape) — simple, sufficient for
this feature's threat model, and not shared cross-cutting abstraction for its
own sake (this crate's established preference — see `applock.rs`'s own doc
comment).

## Protocol

### Discovery (mDNS)

Service type `_openpal._tcp.local.`, advertised only while Remote Access is
enabled. TXT records: `id` (this install's stable random device id), `name`
(display name, currently always `"OpenPal"`), `fp` (first 12 hex chars of the
gateway's TLS certificate fingerprint — a pre-filter only, not a trust
decision), `ver` (protocol version, `"1"`). A client on the same LAN can
browse for this instead of requiring a typed-in IP; over an overlay network
that doesn't propagate multicast, a client falls back to the pairing
payload's `addrs`/manual entry.

### Pairing

Explicit user action only — "Add a device" opens one pairing session (~5
minutes, single-use), never an always-on listener. The desktop shows a QR
code (and a manual fallback code) encoding:

```json
{
  "v": 1,
  "code": "123456",
  "addrs": ["192.168.1.23"],
  "port": 4433,
  "fingerprint": "<sha256 hex of the gateway's TLS cert DER>",
  "mac": "aa:bb:cc:dd:ee:ff",
  "expiresAtMs": 1234567890123
}
```

The client connects to `https://<addr>:<port>/`, pinning `fingerprint`
against the server's actual TLS certificate (trust-on-first-use — the cert is
self-signed by design, so there's no CA to trust instead), then:

**Mismatch behavior** (previously undefined — no client exists yet anywhere
in this repo to have implemented it): hard reject, no partial trust, no
re-prompt-and-continue. `crate::remote::verify_pin` (`crates/core/src/remote/pin.rs`)
is the reference implementation — it compares the presented certificate's
SHA-256-of-DER fingerprint against the pinned one and returns
`Err(PinError::Mismatch)` on any difference; a real client should call this
directly rather than re-deriving the comparison. There is no pin-rotation/
reset flow: a legitimate certificate change (reinstalling OpenPal, a future
"regenerate certificate" admin action) is indistinguishable from a MITM at
this layer, by TOFU's usual trade-off — the only recovery path is re-pairing,
exactly like first-time pairing. `crates/core/tests/remote_tofu_integration.rs`
exercises both the matching and mismatched cases end-to-end over a real TLS
connection to a real gateway.

```
POST /remote/v1/pair
{ "code": "123456", "deviceName": "Sarah's iPhone" }

200 OK
{ "deviceId": "...", "deviceName": "Sarah's iPhone", "token": "<256-bit hex>" }
```

`token` is returned exactly once — store it; it is never retrievable again
(only revocable, from Settings → Advanced on the desktop). A wrong code keeps
the session open for retry within the window; an expired or already-used
session returns `410 Gone` / `409 Conflict` respectively.

### Authenticated API

Every route below requires `Authorization: Bearer <token>`.

```
GET /remote/v1/status
200 OK
{ "engine": <EngineStatus, same tagged shape the desktop UI itself gets> }

POST /v1/chat/completions
<a standard OpenAI-compatible chat-completions body, streaming or not>
```

`/v1/chat/completions` is a raw byte proxy straight to the local engine's own
`/v1/chat/completions` — deliberately bypassing `EngineManager::chat_stream`/
`ChatEvent` entirely. A remote client speaks plain OpenAI-style SSE, not
OpenPal's internal desktop-UI event shape, so there's nothing to translate;
this also means any OpenAI-compatible client library works against it
unmodified.

**If the engine isn't running**, the proxy returns `503 Service Unavailable`
with an actionable message, rather than trying to start it. See "What this
doesn't do" below for why.

### What this doesn't do

- **No engine auto-start from the gateway.** Wake-on-LAN wakes the
  *machine*; getting OpenPal running and a model loaded from there is
  already covered by the OS's own "open at login" and OpenPal's existing
  `Settings.auto_load_on_startup`. Reimplementing `commands::start_engine`'s
  model-selection/GPU-plan/disk-slow logic inside `crate::remote` (which is
  deliberately Tauri-free, like the rest of `openpal-core`) would duplicate
  carefully-tuned, app-layer-only logic. A request that arrives while the
  engine is stopped gets a clear error instead of an attempted auto-start.
- **No per-device rate limiting** (v1 gap, disclosed, not silent). TLS + a
  per-device bearer token + a short-lived single-use pairing code are the
  only defenses once a device is paired. Pairing itself *is* guarded against
  brute force: `/remote/v1/pair` caps wrong-code guesses at
  `pairing::MAX_PAIRING_ATTEMPTS` (5) per session and invalidates the
  session outright once hit, rather than leaving it open for the rest of
  `PAIRING_SESSION_TTL`. That cap has no per-source tracking, so a device on
  the LAN/overlay network flooding guesses during an open session can burn
  through it and kill a legitimate in-progress pairing before the real
  device submits its code — a nuisance (press "Add a device" again), not a
  security hole.
- **Bearer tokens expire after 90 days** (`store::TOKEN_LIFETIME_MS`), not
  never, closing the original "a captured/logged token grants indefinite
  access" gap by two orders of magnitude. 90 days, not something
  Signal-Protocol-short, because this is a LAN/overlay convenience feature
  for a beginner's own devices, not a high-security admin surface — forced
  frequent re-pairing would be beginner-hostile friction for little added
  benefit. Comparison is constant-time (`ring::hmac::verify`, mirroring
  `sync::pairing::verify_mac`'s pattern), not `==`, so a hash mismatch can't
  leak timing information about how many leading bytes matched.
  **There is no in-band token refresh** — no client exists yet anywhere in
  this repo to push a rotated token to, so building one now would be
  speculative complexity ahead of an actual client's needs. Recovery past
  expiry is re-pairing via the desktop's paired-device list, exactly like
  first-time pairing.
- **No per-request replay protection (nonce/counter), deliberately.** This
  feature's transport is already TLS end-to-end — unlike, for example, the
  multi-device sync relay, which explicitly treats the network path as
  untrusted. The real risk here is a captured/logged token being reused
  *indefinitely*, which the 90-day expiry above directly bounds. A
  monotonic nonce would only additionally defend against replaying one
  intercepted request verbatim, which already requires a MITM position that
  TLS plus TOFU certificate pinning (see "Pairing" above) are built to
  prevent — not worth the added client/server state for a beginner-first
  default path.
- **No OpenPal-run relay.** Off-LAN reachability is explicitly "bring your
  own overlay network" — connect both devices to your own Tailscale/
  WireGuard, or configure your router's port-forward + DDNS, then pair while
  connected that way. OpenPal contributes pairing/discovery/WOL and works
  over whatever IP path the overlay provides; it does not operate its own
  internet-facing infrastructure, keeping "local-first, no operated backend"
  intact.
- **No full remote parity.** This is inference only — chat completions.
  Remote settings/library management and cross-device conversation sync are
  out of scope for this feature entirely (conversation sync is a separate
  feature with its own relay-based design — see docs/design/MULTI_DEVICE_SYNC.md).

## Wake-on-LAN

`wol::send_magic_packet(mac, broadcast_addr)` builds and sends the standard
102-byte magic packet (6× `0xFF` + the target MAC repeated 16 times) via UDP
broadcast to port 9. This only wakes a machine within its own LAN broadcast
domain — a magic packet does not cross a router on its own. Reaching a
sleeping desktop from outside its LAN needs either the router to forward the
wake (some support this natively) or an always-on relay-capable device on
that LAN; neither is something OpenPal can guarantee, so this is documented
as a hard limit, not silently assumed to work.

`wol_status::detect()` is a best-effort, per-OS check for whether the active
interface even has WOL enabled — macOS via `pmset -g` (implemented for real,
since macOS is the actual shipping target today); Windows/Linux best-effort
(`ethtool`/`Get-NetAdapterPowerManagement`), not exercised in CI since desktop
packaging for those platforms is a separate, not-yet-done effort (README's
"Platform support", `ARCHITECTURE.md`'s "Adding a platform"). **A detection
failure always reports `Unknown`, never a false `Disabled`** — telling a user
WOL is off when it's actually just undetectable would send them fiddling
with a setting that's already correct.

`wol::list_interfaces()`/`build_magic_packet`/`send_magic_packet` are `pub`
from `crate::remote` specifically so a future mobile client's Rust layer can
call them directly (see below) rather than reimplementing wake-on-LAN in a
second language.

## The mobile client (Android)

Built: `crates/core/src/remote/client/` is a fourth `EngineBackend` impl,
`RemoteBackend` (`crates/core/src/engine/mod.rs`'s seam — see
`docs/design/MOBILE.md`). Android's W^X policy forbids exec'ing a bundled
`llama-server` the way desktop's `SubprocessBackend` does, so on Android
`RemoteBackend` isn't an alternative among several — it's the only backend
that can work at all until a future `InProcessBackend` (on-device inference
via `llama-cpp-2`) exists. `crates/app/src/state.rs`'s `finish_unlock` boxes
it into `EngineManager` only under `#[cfg(target_os = "android")]`; the
module itself is plain, cross-platform, Tauri-free Rust (same posture as
`gateway.rs`), unit/integration-tested against a real TLS-bound gateway on
the normal desktop CI runner.

**Trust model, disclosed explicitly**: v1 has no camera QR scanner (see
"Explicitly not built" below), so pairing is mDNS discovery + a typed-in
6-digit code, not a scanned payload. That makes it trust-on-first-use, not
QR-strength pinning: the client connects to `/remote/v1/pair` trusting
*nothing* yet (`crate::remote::client::tls::CapturingVerifier` accepts any
certificate for that one request), sends the code, and only pins the
certificate's fingerprint — captured from that same handshake — once the
gateway's `200` response proves the code matched
(`crate::remote::client::tls::PinnedVerifier`, used for every request after).
A LAN/overlay-network attacker active during that one pairing request could
intercept the code; the QR flow (fingerprint travels inside the scanned
payload, verified *before* the first byte is sent) doesn't have this
exposure. Acceptable for this feature's threat model (see "Why a
self-contained pairing model" above), but real — not silently equivalent to
QR pinning. Camera scanning closes this gap for real when it lands.

`RemoteBackend::endpoint()` stays at the trait's `None` default, not `Some`
— deliberately, unlike this doc's earlier speculation. `agent::orchestrator`'s
tool-calling loop and the web-search sidecar construct a plain
`reqwest::Client` with no reason to trust this connection's pinned
self-signed cert; wiring `endpoint()` up before that path is tested would
leave a route nobody's verified working. Agent-mode/web-search over a remote
backend is still genuinely valuable future scope, still explicitly **not
v1** — v1 is chat completions only, and `chat_stream` is the only path
wired to the gateway's real HTTP surface.

Practically, what shipped and what didn't:

- `crate::remote::client::discovery::Browser` is the browse counterpart to
  `discovery::Advertisement` (mDNS TXT-record parsing, same `mdns-sd`
  daemon type) — reused directly from `crates/app`'s Tauri commands, not
  reimplemented in TypeScript. Won't find anything from an emulator/VM (its
  virtual networking doesn't pass multicast to the host) — confirmed live
  against the `openpal_test` AVD; a real device on a real LAN is unaffected.
  The manual "enter an address" field in Settings → Remote pairing is real
  UI for this and for overlay-network users, not a test-only fallback.
- **Android**: `CHANGE_WIFI_MULTICAST_STATE` + `ACCESS_WIFI_STATE` permissions
  and a `WifiManager.MulticastLock`, held only while `Browser` is running
  (`WifiLockPlugin.kt` + `crates/app/src/wifi_lock_android.rs`, mirroring
  `SecretStorePlugin.kt`'s shape) — confirmed live via `adb logcat`
  (`WifiService: acquireMulticastLock`) that this is a real OS-level lock,
  not a no-op. Wake-on-LAN itself isn't wired up on mobile yet (no UI calls
  `wol::send_magic_packet` there today).
- **Foreground-only discovery** — `remote_stop_discovery` releases the lock
  and tears down the browser; nothing runs in the background.
- **Explicitly not built**: camera QR scanning (no scanning capability
  exists anywhere in this codebase yet — see the trust-model note above),
  agent-mode/web-search over `RemoteBackend`, `raw_classify` (no
  `/remote/v1/classify`-style route exists on the gateway — the request-
  triage and safety-banner features are silently unavailable on this
  backend, see `docs/design/MOBILE.md`'s cut-features table), and iOS (this work
  is Android-only throughout; iOS still needs the
  `NSLocalNetworkUsageDescription`/`NSBonjourServices` `Info.plist` entries
  this doc already called out, and is blocked on Xcode access regardless).

## Adding a platform

Desktop-side server logic (`gateway.rs`, `wol.rs`, `wol_status.rs`,
`discovery.rs`) is written cross-platform (no macOS-only APIs in the
send/serve paths; `wol_status.rs`'s per-OS detection follows this crate's
existing `#[cfg(target_os = ...)]` convention). macOS is the actual shipping
target and the only platform this has been run/tested on; Windows/Linux
desktop packaging is a separate, not-yet-done effort — this feature doesn't
change that timeline, it just doesn't add new macOS-only assumptions to it.
