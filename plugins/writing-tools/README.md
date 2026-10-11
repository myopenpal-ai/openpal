# Writing Tools (writing-tools) plugin

One-shot Translate/Correct/Improve screens that run on the model already
loaded — never a second, translation-specialized model. Paste (or type a
draft) in, get a streamed result out; nothing is saved to Library or
conversation history.

## How it fits together

This is an OpenPal plugin (see `../PLUGINS.md`) in name only — it has **no**
`python` block. It exists purely as an opt-in switch: enabling it advertises
the `writing-tools` capability, which reveals the "Translate"/"Correct"
sidebar entries plus the composer's inline "Improve my message" button and
the standalone "Improve a message" screen. All the actual work lives in the
main app — `crates/core/src/writing_tools.rs`'s Translate/Correct request
builders and `crates/core/src/prompt_enhancer.rs`'s Improve prompt/cleaner
(fixed system prompt + low-temperature sampling per mode, never `Settings.
system_prompt`), the `translate_stream`/`correct_stream`/`enhance_prompt`
Tauri commands (`crates/app/src/commands.rs`), and
`ui/src/views/WritingToolsScreen.tsx`/`ImproveView.tsx`. See §8 of
`../PLUGINS.md` for why a UI-only capability flag is a legitimate plugin.

Unlike `smart-model-router`, this capability gates a UI surface only:
`translate_stream`/`correct_stream`/`enhance_prompt` are net-new commands
with no other call site, so they are deliberately **not** gated by
`PluginManager::capability_enabled` — same as `hf-browser`'s ungated HF
commands (see `docs/design/HF_BROWSER.md`'s decision 2). `enhance_prompt` used to
be gated by its own `Settings.prompt_enhancer_enabled` toggle instead; that
field is gone now that Improve shares this plugin's single on/off switch
with Translate/Correct.

It's a plugin (rather than an always-visible core nav item) so beginners
never see it: OpenPal's default experience is one clean Chat view; Writing
Tools is the opt-in escape hatch for anyone who wants dedicated
translate/correct tooling without cluttering the default UI.

## Why python-less plugins are excluded from packaged Portable builds

Nothing here needs a host Python or network setup step, but Portable mode
skips *every* built-in plugin uniformly (see `docs/design/PORTABLE.md`) — dropping
this folder into `<data>/plugins/` on a Portable install still works if you
want it there.
