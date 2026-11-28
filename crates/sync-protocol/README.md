# openpal-sync-protocol

Wire types shared by OpenPal clients and sync services, so the two sides
cannot drift silently out of sync:

- `Envelope` — the mailbox store-and-forward primitive.
- `vault` — the durable, account-scoped vault requests, responses and error
  envelope, with JSON fixtures under [`fixtures/vault/`](fixtures/vault/).

`cargo test -p openpal-sync-protocol` round-trips every vault fixture and
fails when a fixture is not referenced, so a field change is caught here
before it reaches a client or a server.

The vault wire is additive-only. See [CHANGELOG.md](CHANGELOG.md) for the
rule and what changed when.
