# Remote Inference (remote-inference) plugin

Point OpenPal at another OpenPal device's engine instead of this computer's,
so a model too big for this machine's RAM/GPU becomes usable. Replies stream
into the composer over the same encrypted, authenticated (bearer-token over
pinned TLS) connection the Remote Access gateway already serves; request
triage and the safety/risk check run against that same remote engine, exactly
as they do locally.

## How it fits together

This is an OpenPal plugin (see `../PLUGINS.md`) in name only — it has **no**
`python` block. It exists purely as an opt-in switch: enabling it advertises
the `remote-inference` capability, which reveals the "Remote inference"
Settings section under Devices (desktop only). All the actual work lives in
the main app — the section's connect flow pairs with another OpenPal device
(reusing `RemoteBackend`'s pairing/pinned-TLS/bearer-token machinery) and then
routes the whole engine surface (`EngineManager`) over to that device, exactly
as Android already does permanently via `EngineManager::with_backend` — see
`plugins/PLUGINS.md` §8 for why a UI-only capability flag is a legitimate
plugin, and its extension for backend-gated capabilities
(`PluginManager::capability_enabled`).

Nothing listens or connects while the plugin is off: this feature has no
service to auto-start, no background task, and `EngineManager` starts on the
local engine as always. Connect is an explicit user action, finally ended by
Disconnect (which also forgets the pairing), by disabling the plugin, or by
turning experimental features back off.

## Why python-less

Like `hf-browser`/`smart-model-router`, remote inference needs no host Python
or network setup step — the pairing is pure Rust + the pinned TLS client
(`crates/core/src/remote/client/`), and the Settings section is pure
TypeScript. See `../PLUGINS.md` §8 (and `docs/design/HF_BROWSER.md`'s decision
notes) for the same reasoning.
