# Smart Model Router (smart-model-router) plugin

Auto-picks the best already-*installed* model for whatever the user is
typing — coding, writing, math/science, or agentic — instead of staying on
one model all the time. It never suggests downloading something new, and it
never switches mid-generation.

## How it fits together

This is an OpenPal plugin (see `../PLUGINS.md`) in name only — it has **no**
`python` block. It exists purely as an opt-in switch: enabling it advertises
the `smart-model-router` capability, which turns on the model-switching half of
request-triage. All the actual work lives in the main app —
`crates/core/src/types.rs`'s `ModelSpec.task_scores` (hand-curated per-model
fit scores), `ui/src/lib/triage.ts`'s `bestModelForTag` (the ranking logic),
and `ChatView.tsx`'s `useTriageAutopilot` (the debounce/stability/pin
machinery that decides *when* to actually switch) — see §8 of
`../PLUGINS.md` for why a UI-only capability flag is a legitimate plugin, and
its extension for backend-gated capabilities (`PluginManager::capability_enabled`,
used by `classify_message`).

It's a plugin (rather than an always-on setting) so multi-model auto-pick
stays opt-in: OpenPal's default experience is one model, picked/switched by
hand; Smart Model Router is the escape hatch for anyone who's downloaded several
models and wants OpenPal to route between them automatically. Switching
models means a full engine reload (seconds to minutes, per `engine.rs`), so
the switch logic is deliberately conservative — see `ARCHITECTURE.md`'s
request-triage section for the exact timing/stability rules.

## Why python-less plugins are excluded from packaged Portable builds

Nothing here needs a host Python or network setup step, but Portable mode
skips *every* built-in plugin uniformly (see `docs/design/PORTABLE.md`) — dropping
this folder into `<data>/plugins/` on a Portable install still works if you
want it there.
