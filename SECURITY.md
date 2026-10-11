# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for a security problem. Report it
privately through GitHub instead:
[**Report a vulnerability**](https://github.com/myopenpal-ai/openpal/security/advisories/new)
(Security tab → Advisories → Report a vulnerability).

Include what you found, how to reproduce it, and the impact you think it has.
We'll acknowledge the report, keep you updated while we work on a fix, and
credit you in the advisory unless you'd rather stay anonymous.

## Supported versions

OpenPal is pre-1.0. Fixes land on `main` and in the next release; older builds
aren't patched.

## Areas of particular interest

OpenPal runs AI models locally and some features reach beyond the app's own
sandbox. These are the places where a bug matters most:

- **Agent mode** — lets the model run shell commands and read/write files,
  jailed to a per-conversation workspace ([docs/AGENT_MODE.md](docs/AGENT_MODE.md)).
  Escaping the jail, or getting a tool call to run without the user turning
  agent mode on, is in scope.
- **Remote access** — when switched on, the desktop serves inference to paired
  devices over the network ([docs/REMOTE_INFERENCE.md](docs/REMOTE_INFERENCE.md)).
  Pairing bypasses and unauthenticated access are in scope.
- **Multi-device sync and pairing** — end-to-end encrypted sync and its pairing
  protocol ([docs/SYNC_PAIRING_PROTOCOL.md](docs/SYNC_PAIRING_PROTOCOL.md)).
- **Downloads and updates** — models, the llama.cpp engine and app updates
  fetched from the internet.
- **Plugins** — Python sidecar services started by the app
  ([plugins/PLUGINS.md](plugins/PLUGINS.md)).
- **Local data at rest** — the encrypted app database and logs.

The self-hosted sync relay (`crates/relay`) deliberately has **no TLS and no
authentication** of its own; it only ever sees ciphertext and expects a reverse
proxy in front of it ([docs/RELAY_SELF_HOSTING.md](docs/RELAY_SELF_HOSTING.md)).
Reports that boil down to "the relay has no auth" are expected behavior, but
anything that lets the relay or a network attacker read or forge sync data is
in scope.
