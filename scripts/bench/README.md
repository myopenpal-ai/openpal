# DiskSlow speed-lever benches (16 GB M4 series, 2026-07-08 → 2026-11-08)

Standalone scripts that drive the bundled `llama-server` with the exact
`start_engine` DiskSlow recipe (`-ngl 0 -c 8192 --no-warmup`) — same method as
the Phase 3 go/no-go bench in `docs/design/DISK_OFFLOAD.md`, where all results are
recorded. Output goes to `$BENCH_OUT_DIR` (default `/tmp/openpal-bench`):
`*_results.jsonl` has llama-server's own timings per config.

- `bench_mtp.sh` — baseline + `--spec-type draft-mtp` A/B on glm-4.5-air.
  RAN 2026-07-08: baseline 0.127/0.124 tok/s; **draft-mtp crashes b9833**
  with the Unsloth GGUF (unmapped `nextn` tensors) — do not ship.
- `bench_round2.sh` — RAN in full 2026-07-11: `ngram-mod` TG 0.122 tok/s (no
  win, twice-confirmed); `-ub 2048` PP A/B **10.65 vs 5.04 tok/s** on a
  2354-token prompt (2.1×, TG unchanged); `-fa on -ctk q8_0 -ctv q8_0` TG
  0.153 tok/s (no regression). The `-ub`/KV-quant winners are wired into
  `start_engine`'s DiskSlow recipe.
- `bench_round3.sh` — PARTIALLY RUN: the gpt-oss-20b download (12.11 GB,
  sha256-verified) and near-fit `-ngl 0` baseline were split out and run
  2026-07-11: **3.68 tok/s** generation, 4.29 tok/s PP, healthy in ~45 s —
  within ~11% of the estimator's ~4.1 prediction. The
  `GGML_METAL_NO_RESIDENCY=1 -ngl 99 --cpu-moe` Metal experiment on
  gpt-oss-20b and glm-4.5-air (wired-memory watchdog kills the server past
  13.5 GB) is STILL PENDING — run it only when the machine can be
  sacrificed; it probes the llama.cpp #19825 kernel-panic neighborhood,
  deliberately.
- `bench_qwen_mtp.sh` — baseline vs `--spec-type draft-mtp` at depths
  2/3/4 on qwen3.6-27b (Q4_K_M + bf16 mmproj, 17.1 GB + 0.9 GB), exact
  DiskSlow recipe, engine **b11960**. RAN 2026-11-08: baseline did **not
  finish** within the 60-min per-request cap (recorded as an upper bound
  of <0.036 tok/s); `draft-mtp` won monotonically with depth — **depth 2:
  0.050 tok/s** (draft_n 90, 81 accepted, mean len 2.80) · **depth 3:
  0.062** (110/89, 3.41) · **depth 4: 0.074** (128/95, 3.97). Single-rep
  measurement (round-2 reps stopped mid-run by user decision). Verdict
  that shipped the feature: `start_engine` now arms self-drafting for the
  `qwen3.6-27b` entry (`--spec-draft-n-max 4`) when the current engine
  binary advertises `draft-mtp`. The same script's GLM-4.5-Air probe
  showed b11960 no longer crashes on `draft-mtp` but gains nothing
  (0.162–0.178 vs 0.181 baseline) — that entry stays un-armed.

Paths (engine binary, models dir) are hardcoded for the dev Mac at the top of
each script; adjust for other bench machines (32 GB Mac, Win/Linux + GPU).
