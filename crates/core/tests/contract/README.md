# Vendored cloud `/v1` contract

This is a byte-for-byte copy of the versioned contract fixture set from the
private backend repo (`myopenpal-ai/openpal-cloud`, `contract/`), dated
`2026-11-25`. It is committed here so `cargo test -p openpal-core` can replay
it with no credential, network call, or repository secret — including from a
fork's `pull_request` run.

- `MANIFEST.toml` is the backend's own inventory: one entry per fixture, its
  route, status, content type, the type it is parsed as, and any dynamic
  placeholders. [`crate::contract_tests`](../../src/contract_tests.rs) drives
  its loop from this file, so a fixture the manifest lists is exercised, and a
  file it does not list fails the test.
- `PIN.toml` records the version, source and a digest of every byte under
  `v1/`. Editing a fixture in place fails the digest test; re-vendoring the
  whole set and updating the pin is the only way through.
- `CHANGELOG.md` is the backend's contract changelog, included so a future
  reader can see what a version bump changed.
- `v1/` is the fixtures themselves.

`v1/search_results.json` is the one fixture no shipped client calls
(`/v1/managed-search/search` is not wired up yet), so the replay skips it by
name, with that reason, rather than silently ignoring it.

There are three suites, deliberately separate:

- `crate::contract_tests` (this directory) replays the recorded bytes through
  the shipped parsers. It proves the client reads the contract.
- `crate::contract_smoke` drives the real `AccountManager`/`cloud::chat_stream`
  against the backend's dev-only double and asserts its `x-openpal-contract`
  header matches this `PIN.toml`. It proves both implementations agree end to
  end; CI's `contract-smoke` job runs it.
- The backend's own fake-server tests prove the server emits the fixtures.

None replaces another: the replay is hermetic and runs in `cargo test`, the
smoke test needs the digest-pinned image, and the backend suite runs in the
private repo.
