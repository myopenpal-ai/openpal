# Changelog — openpal-sync-protocol

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- A public `vault` module with the durable-store wire: idempotent vault
  create, versioned keyring get/put with provenance, compare-and-swap item
  put (including conditional tombstones via `deleted: true`), opaque-cursor
  item pull, usage, and the shared error envelope and codes. Item delete is
  the backend spec's bodyless `DELETE … → 204` writing a tombstone, so it has
  no JSON request/response type.
- The item kind `setting` (singular, matching `docs/VAULT.md` §6).
- `VAULT_PROTOCOL_VERSION`, next to `PROTOCOL_VERSION`.
- JSON fixtures under `fixtures/vault/` and round-trip tests that fail on an
  unreferenced fixture.

### Rules

- **Additive-only.** A new optional field may be added at any time; readers
  ignore fields they do not know and writers omit optional fields they do not
  use. An existing field is never renamed, removed or given a new meaning.
- `VAULT_PROTOCOL_VERSION` changes only for a breaking wire change (a new
  route or a shape an older reader cannot parse). Additive fields and new
  error codes do not bump it.
- A new item kind or a new error code needs a fixture and a client parser
  test before a server emits it.
