# Catalog model specs — source verification

`crates/core/data/catalog.json`'s `moe`/`paramsB`/`activeParamsB` fields feed directly into the
token/sec speed estimate (`ui/src/lib/speedEstimate.ts`, see
[`SPEED_ESTIMATE_ACCURACY.md`](./design/SPEED_ESTIMATE_ACCURACY.md)): for a MoE entry the estimate scales
by `activeParamsB / paramsB`, so a wrong active-params figure is a wrong speed estimate by that
same factor. The catalog is a small, fixed list of default models — rather than adding runtime
verification machinery for something that never changes once shipped, this is a one-time audit:
every entry checked against official sources and the figures hardcoded here for the record.

Discover (Hugging Face) downloads and manually-added local files don't need this — they already
get exact `moe`/`activeParamsB` at runtime by parsing the model's own GGUF header
(`crates/core/src/gguf_meta.rs`; see "Not in the catalog" below).

## Verdict

All 19 curated entries checked out. **No `catalog.json` changes were needed** — every `moe`,
`paramsB`, and `activeParamsB` value already matched its official source (within normal rounding).
This file exists so that fact is on record with citations, and as a template for verifying any
model added to the catalog in the future.

## MoE entries (5)

| id | total (B) | active (B) | experts (total/used) | source |
|---|---|---|---|---|
| `gpt-oss-20b` | 20.9 | 3.6 | 32 / 4 | [openai/gpt-oss-20b](https://huggingface.co/openai/gpt-oss-20b) |
| `qwen3-30b-a3b-instruct-2507` | 30.5 | 3.3 | 128 / 8 | [Qwen/Qwen3-30B-A3B-Instruct-2507](https://huggingface.co/Qwen/Qwen3-30B-A3B-Instruct-2507) |
| `qwen3.5-35b-a3b` | 35 (catalog: 36, within rounding) | 3 | 256 / 8 + 1 shared | [Qwen/Qwen3.5-35B-A3B](https://huggingface.co/Qwen/Qwen3.5-35B-A3B) |
| `glm-4.7-flash` | 31.22 | 3.6 | 64 / 4 + 1 shared | [zai-org/GLM-4.7-Flash](https://huggingface.co/zai-org/GLM-4.7-Flash) config.json; active figure from [Unsloth's docs](https://unsloth.ai/docs/models/tutorials/glm-4.7-flash) (the official card doesn't state one) |
| `glm-4.5-air` | 106 | 12 | 128 / 8 + 1 shared | [zai-org/GLM-4.5-Air](https://huggingface.co/zai-org/GLM-4.5-Air) |

All five publishers state total/active parameters directly on the model card — that's the figure
used, not a hand-derived one, since it already accounts for shared experts and attention/embedding
layers correctly.

## Dense entries worth individual scrutiny (4)

These looked risky enough (an "effective-params" naming pattern, or a post-training-cutoff
version bump) to warrant checking individually rather than trusting the size/name alone.

| id | total (B) | verdict |
|---|---|---|
| `gemma-4-e2b` | 4.65 (catalog: 4.6) | Dense. "E2B" = MatFormer/Per-Layer-Embeddings "effective 2B", not MoE — see naming trap below. |
| `gemma-4-e4b` | 7.52 (catalog: 7.5) | Dense, same reason. |
| `gemma-4-12b` | 11.95 (catalog: 12.0) | Dense — `config.json` explicitly sets `enable_moe_block: false`. |
| `qwen3.6-27b` | 27 (catalog: 27.3) | Dense — hybrid Gated-DeltaNet/Attention, no expert fields in `config.json`. **Ships MTP (multi-token prediction) heads usable by the bundled engine for speculative self-drafting** — bench-verified 2026-11-08 (b11960, exact DiskSlow recipe: depth-4 `draft-mtp` 0.074 tok/s vs <0.036 baseline upper bound); entry is `mtp: true`, so `start_engine` arms `--spec-type draft-mtp --spec-draft-n-max 4` for it (see `docs/design/DISK_OFFLOAD.md` round 4). |

Sources: official `google/gemma-4-{E2B,E4B,12B}-it` and `Qwen/Qwen3.6-27B` Hugging Face repos
(README + `config.json`), cross-checked against the GGUF files OpenPal actually ships
(`ggml-org/gemma-4-*-it-GGUF`, `unsloth/Qwen3.6-27B-MTP-GGUF`).

## `releaseDate` (added for the "All models" table, `ui/src/views/Library.tsx`)

12 of 19 entries got a `releaseDate` (well-documented, widely-reported launch dates for models this
audit already treats as real, citable releases): `llama-3.2-1b`/`llama-3.2-3b` (2024-09-25),
`llama-3.1-8b` (2024-07-23), `llama-3.3-70b` (2024-12-06), `gemma-3-4b` (2025-03-12), `gpt-oss-20b`
(2025-08-05), `qwen3-8b`/`qwen3-14b`/`qwen3-32b` (2025-04-29, the original Qwen3 launch),
`qwen3-4b-instruct-2507`/`qwen3-30b-a3b-instruct-2507` (2025-07, the "-2507" refresh wave — month
only, day not independently confirmed), `glm-4.5-air` (2025-07, month only).

The remaining 7 (`smollm2-135m` — hidden test model, out of scope; `gemma-4-e2b`/`gemma-4-e4b`/
`gemma-4-12b`; `qwen3.6-27b`; `qwen3.5-35b-a3b`; `glm-4.7-flash`) were **deliberately left without
one** — this pass could confirm `moe`/`paramsB` for them against `config.json`, but found no
independently-dated announcement to cite for *when* they shipped, and this file's own discipline is
to record only what's actually been verified rather than a plausible-looking guess. The UI shows
"—" for these until a future pass finds a real date to cite. Do not backfill a date here without
a source line the way every other entry in this file has one.

## Well-known dense entries (10)

Batch-checked for completeness; none had any real chance of being wrong (long-established, widely
documented releases with no same-name/same-size MoE sibling to confuse them with):
`smollm2-135m`, `llama-3.2-1b`, `llama-3.2-3b`, `gemma-3-4b`, `qwen3-4b-instruct-2507`,
`llama-3.1-8b`, `qwen3-8b`, `qwen3-14b`, `qwen3-32b`, `llama-3.3-70b`. All confirmed dense, all
`paramsB` figures matched their official model cards.

## Naming traps hit during this audit

- **"E<n>B" ≠ MoE.** Gemma's `E2B`/`E4B` naming (introduced in Gemma 3n, continued in Gemma 4)
  means "effective <n>B" — a MatFormer/Per-Layer-Embeddings dense architecture where Google
  publishes a compute-proxy parameter count smaller than the checkpoint's real weight count. It
  is not sparse expert routing and has no `expert_count`/`expert_used_count` in `config.json`.
  Real MoE variants use "A\<n\>B" ("active \<n\>B") instead — e.g. Gemma 4's actual MoE model is a
  separate `26B-A4B` release (128 experts, 8 used, ~3.8B active of ~25.2B total), not curated here.
- **"-MTP" in a filename ≠ MoE.** `unsloth/Qwen3.6-27B-MTP-GGUF`'s "MTP" is Multi-Token
  Prediction (a speculative-decoding technique), unrelated to expert routing. Qwen3.6-27B is
  fully dense.
- **Version numbers matter.** `qwen3.5-35b-a3b` (curated, MoE, verified above) and
  `qwen3.6-27b` (curated, dense, verified above) are two different Qwen releases; a third,
  `Qwen3.6-35B-A3B` (MoE, same size class as the curated 3.5 model), also exists on Hugging Face
  but is not in this catalog. Easy to conflate by size/shape alone — check the exact repo.
- **A publisher's "effective" params aren't necessarily what drives memory-bandwidth-bound
  tok/s.** One research pass flagged that Gemma 4 E4B's PLE design might make real tok/s faster
  than a naive dense (bandwidth ÷ full file size) estimate predicts, since "effective 4.5B"
  is smaller than the shipped 7.5B checkpoint. It isn't, in practice — the estimate landed dead-on
  against the user's own measured benchmark (16 est. vs 16 real) with the catalog's current
  `activeParamsB: null` (fully dense) treatment. PLE apparently saves FLOPs, not the memory
  traffic that gates single-batch generation speed — so E2B/E4B are left as dense entries here,
  deliberately not "corrected" toward their marketing figure.

## Not in the catalog

Two real MoE models surfaced during this audit that aren't curated: Gemma 4 `26B-A4B` (128
experts, 8 used, ~3.8B active) and `Qwen3.6-35B-A3B`. Both match the worst two error cases in the
original speed-estimate bug report almost exactly by shape (a 26B-class and a 35B-class sparse
MoE). Since neither is curated, OpenPal never trusted a hardcoded figure for them — a Discover
download range-fetches the model's own GGUF header and computes `moe`/`activeParamsB` exactly
(`crates/core/src/gguf_meta.rs`, wired through `commands::get_hf_quant_picker`); a manually-added
local file parses its own header the same way, synchronously, off disk
(`custom::local_spec`). No action needed here — this section just records that the two
worst-offending models from the original report were already covered by that machinery, and the
9.8×/40× estimate errors reported for them came from the fit-blindness bug fixed separately (see
`SPEED_ESTIMATE_ACCURACY.md`), not from bad sparsity detection.

## Task defaults (sampling recommendations)

`catalog.json` entries can also carry a `taskDefaults` map (one key per
`TaskType`) of curated `SamplingParams` — consulted by
`Catalog::recommended_sampling` / the frontend's `recommendedSampling`
(`ui/src/lib/types.ts`) to seed a new conversation's Generation panel
instead of one flat app-wide default, and to auto-tune it as request-triage
classifies what the user is typing. A model with no `taskDefaults` entry
for the current task (every Hugging Face / local-file model, and any
catalog entry without a documented recommendation) falls back to
`general_sampling_defaults` — this app's own model-agnostic default,
reasonable for anything not called out below.

A model with a hybrid thinking/non-thinking mode can additionally carry a
`taskDefaultsThinking` map, consulted first whenever `Settings.showThinking`
is on and it has an entry for the current task — `taskDefaults` then doubles
as that model's instruct/non-thinking-mode table.

Unlike the `moe`/`paramsB`/`activeParamsB` audit above, these are **not**
individually verified against each model's own model card — several of
these catalog entries (`qwen3.5-35b-a3b`, `qwen3.6-27b`, the `gemma-4-*`
line, `glm-4.7-flash`) are past this assistant's training cutoff, so their
exact official generation config can't be confirmed here. Every entry
except Gemma 4, Qwen3.5, and Qwen3.6 (below) uses its **family's
well-established, long-published convention**:

| family / variant | temperature | topP | topK | minP | applies to |
|---|---|---|---|---|---|
| Llama 3.x | 0.6 | 0.9 | (default) | (default) | `llama-3.2-1b`, `llama-3.2-3b`, `llama-3.1-8b`, `llama-3.3-70b` |
| Qwen3 hybrid think/non-think (thinking mode, this app's default) | 0.6 | 0.95 | 20 | 0 | `qwen3-8b`, `qwen3-14b`, `qwen3-32b` |
| Qwen3 "-Instruct-2507" (non-thinking-only releases) | 0.7 | 0.8 | 20 | 0 | `qwen3-4b-instruct-2507`, `qwen3-30b-a3b-instruct-2507` |
| Gemma (2/3) | 1.0 | 0.95 | 64 | (default) | `gemma-3-4b` |
| GLM-4.5+ | 0.6 | 0.95 | (default) | (default) | `glm-4.5-air`, `glm-4.7-flash` |
| gpt-oss | 1.0 | 1.0 | (default) | (default) | `gpt-oss-20b` |

"(default)" means that knob is left at llama.cpp's own default (`topK: 40`,
`minP: 0.05`) — none of these publishers document a specific recommendation
for it. `smollm2-135m` (hidden test model) has no override. Every other
`SamplingParams` field (`repeatPenalty`, `repeatLastN`, `presencePenalty`,
`frequencyPenalty`, `maxTokens`, `seed`) is left at its llama.cpp default
across every entry above — none of those families publish guidance for those.

**Gemma 4** (`gemma-4-e2b`, `gemma-4-e4b`, `gemma-4-12b`): directly supplied
guidance gives one flat preset across every use case — `temperature: 1.0,
topP: 0.95, topK: 64` — applied to all five `TaskType`s alike (no
`taskDefaultsThinking`; Gemma has no thinking mode). `gemma-3-4b` keeps the
family-convention row above rather than this newer, model-specific data.

**Qwen3.5** (`qwen3.5-35b-a3b`) and **Qwen3.6** (`qwen3.6-27b`): directly
supplied guidance splits by thinking vs. instruct mode, and (for Qwen3.5)
further by task:

| model | mode | task | temperature | topP | topK | minP | presencePenalty | repeatPenalty |
|---|---|---|---|---|---|---|---|---|
| Qwen3.5 | thinking | general | 1.0 | 0.95 | 20 | 0 | 1.5 | 1.0 |
| Qwen3.5 | thinking | coding | 0.6 | 0.95 | 20 | 0 | 0.0 | 1.0 |
| Qwen3.5 | instruct | general | 0.7 | 0.8 | 20 | 0 | 1.5 | 1.0 |
| Qwen3.5 | instruct | mathScience (reasoning) | 1.0 | 0.95 | 20 | 0 | 1.5 | 1.0 |
| Qwen3.6 | thinking | general | 1.0 | 0.95 | 20 | 0 | 0.0 | 1.0 |
| Qwen3.6 | thinking | coding | 0.6 | 0.95 | 20 | 0 | 0.0 | 1.0 |
| Qwen3.6 | instruct | any task | 0.7 | 0.80 | 20 | 0 | 1.5 | 1.0 |

Task/mode combinations not listed above (e.g. Qwen3.5 instruct+coding, or
either model's `writing`/`agentic` in either mode) have no curated entry and
fall through the normal chain (`taskDefaultsThinking` → `taskDefaults` →
`general_sampling_defaults`) — no data was supplied for them, so nothing is
fabricated.

If a future model's *actual* published recommendation is confirmed to
differ from its family's convention (or this directly-supplied data), update
its `taskDefaults`/`taskDefaultsThinking` entry and this section together.

## Model-based routing (`smart-model-router` plugin)

Every visible entry also carries a `taskScores` map — one 0-100 fit score
per `TaskType`, answering a different question from `taskDefaults` above:
not "what sampling should *this* model use for this task", but "how *good*
is this model at this task, relative to the rest of the catalog". This is
what the opt-in `smart-model-router` plugin's model auto-pick
(`ui/src/lib/triage.ts`'s `bestModelForTag`, consulted only when the
plugin is enabled — see `plugins/smart-model-router/`) ranks installed models by.
Multi-model auto-pick lives in a plugin rather than baked into the core app
so OpenPal's default experience stays single-model (the user picks/switches
models by hand); `taskScores` itself is core catalog data either way, same
posture as `qualityScore` — always present, whether or not the plugin is
installed/enabled.

Hand-curated relative rankings, same posture as `qualityScore` — not
individually benchmarked. `general`/`coding`/`writing` are grounded in each
entry's existing `description`/`strengths` fields (e.g. GLM-4.7-Flash and
GLM-4.5-Air are already described above as coding/agentic-strong; the Gemma
line as writing/many-languages-strong). `mathScience` and `agentic` are
newer categories with less textual grounding in the catalog's own copy, so
they lean more on each model family's public reputation instead:
- **`mathScience`** (math/science reasoning): the Qwen3 line, GLM-4.5+, and
  gpt-oss score highest — all three families are widely reported to do well
  on math/reasoning benchmarks; the smaller Llama 3.2 and Gemma entries score
  lowest.
- **`agentic`** (tool-use / function-calling / benchmark-style agentic
  tasks): GLM-4.7-Flash and GLM-4.5-Air score highest (both explicitly
  marketed as agentic specialists — see their `description`/`strengths`
  above) alongside gpt-oss-20b (OpenAI's open model, built around reliable
  tool calling); the Llama 3.x and smaller Gemma entries score lowest — none
  of those are specifically tuned for structured tool-call output.

If a future model's real agentic/math benchmark results (e.g. a published
BFCL or GSM8K/MATH score) become available, prefer them over this
reputation-based estimate and update its `taskScores` entry accordingly.

## Verifying a new catalog entry

When adding a model to `catalog.json`:
1. Find the *official* base-model Hugging Face repo (not a GGUF re-upload) and read its README
   and `config.json`. Most current MoE releases state "Total Parameters" / "Activated Parameters"
   directly — use that figure, don't hand-derive it.
2. If unsure whether it's MoE, check `config.json` for `num_experts`/`num_local_experts` and
   `num_experts_per_tok`/`top_k_experts` (naming varies by architecture family). Their absence (or
   explicit `null`/`enable_moe_block: false`) means dense.
3. Watch for the naming traps above — "effective" params, "-MTP" suffixes, and off-by-one-version
   confusion between similarly-sized releases.
4. If the official card doesn't state an active-params figure, the GGUF quantizer's own docs
   (Unsloth, ggml-org) are a reasonable secondary source — they usually restate it.

Audited 2026-07-19.
