# Model Browser (hf-browser) plugin

Adds a **Browse** screen that searches and browses trending GGUF models on
[Hugging Face](https://huggingface.co) right inside OpenPal — search, sort by
trending/downloads/likes, open a model's card (metadata, README, quant
picker), and add a file to your library. Downloading still goes through the
same resumable pipeline as every other model in Models.

Everything runs on your Mac except the Hugging Face API calls themselves
(there's no way around that — it's a hub browser). No account, no tracking:
requests are anonymous, public, read-only lookups.

## How it fits together

This is an OpenPal plugin (see `../PLUGINS.md`) in name only — it has **no**
`python` block. It exists purely as an opt-in switch: enabling it advertises
the `model-browser` capability, which reveals the Browse sidebar entry and
view. All the actual work (the Hugging Face API client, the Tauri commands,
the Browse UI) lives in the main app (`crates/core/src/huggingface.rs`,
`crates/app/src/commands.rs`, `ui/src/views/Discover.tsx`) — see §8 of
`../PLUGINS.md` for why a UI-only capability flag is a legitimate plugin.

It's a plugin (rather than an always-on Library tab) so it stays invisible by
default — OpenPal's curated model catalog is the beginner path, and browsing
Hugging Face directly is a power-user escape hatch you opt into. See
`docs/design/HF_BROWSER.md` for the full design.

## Why python-less plugins are excluded from packaged Portable builds

Nothing here needs a host Python or network setup step, but Portable mode
skips *every* built-in plugin uniformly (see `docs/design/PORTABLE.md`) — dropping
this folder into `<data>/plugins/` on a Portable install still works if you
want it there.
