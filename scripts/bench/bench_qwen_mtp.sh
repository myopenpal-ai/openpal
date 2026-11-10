#!/bin/bash
# MTP self-draft A/B on qwen3.6-27b, exact DiskSlow recipe + vision mmproj
# (-ngl 0 -c 8192 --no-warmup -b 2048 -ub 2048 -fa on -ctk q8_0 -ctv q8_0 --mmproj).
#
# Baseline vs --spec-type draft-mtp (self-drafting with the model's own MTP
# heads). Files are downloaded into the models dir and sha256-verified against
# the catalog entries before any server starts. See issue #6 and
# docs/design/DISK_OFFLOAD.md.
#
# Configs (each replicated `REPS` times for noise):
#   base   — no spec flag (the current production start)
#   mtp2.. — --spec-type draft-mtp --spec-draft-n-max N for N in {2,3,4}
#
# Env overrides: LLAMA_BIN, OPENPAL_MODELS_DIR, BENCH_OUT_DIR, REPS, NPRED.
set -u
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LLAMA_BIN:-$REPO_ROOT/crates/app/resources/llama/llama-server}"
MODELS_DIR="${OPENPAL_MODELS_DIR:-$HOME/Library/Application Support/com.myopenpal.openpal/models}"
OUT_DIR="${BENCH_OUT_DIR:-/tmp/openpal-bench}"
REPS="${REPS:-2}"
NPRED="${NPRED:-128}"

mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/qwen_mtp_bench.log"
RESULTS="$OUT_DIR/qwen_mtp_bench_results.jsonl"

# From crates/core/data/catalog.json (qwen3.6-27b entry) — keep in sync.
MODEL_NAME="Qwen3.6-27B-Q4_K_M.gguf"
MODEL_URL="https://huggingface.co/unsloth/Qwen3.6-27B-MTP-GGUF/resolve/main/$MODEL_NAME"
MODEL_SHA256="a7cbd3ecc0e3f9b333edee61ae66bc87ed713c5d49587a8355814722ed329e0f"
MMPROJ_NAME="Qwen3.6-27B-mmproj-bf16.gguf"
MMPROJ_URL="https://huggingface.co/unsloth/Qwen3.6-27B-MTP-GGUF/resolve/main/mmproj-BF16.gguf"
MMPROJ_SHA256="05353347512982ee62317b9d8c89372bc815f4b4043580e7ef3ad411ec1a1cd3"

MODEL="$MODELS_DIR/$MODEL_NAME"
MMPROJ="$MODELS_DIR/$MMPROJ_NAME"
PORT=18081

PROMPT="Explain in plain language why the sky appears blue during the day and reddish at sunset, and mention one common misconception people have about this phenomenon."

# Download path into the models dir, resumable, and verify sha256 against the
# catalog entry. Re-downloads on mismatch. Fails the script on failure.
download_and_verify() {
  local url="$1" dest="$2" expected_sha="$3" label="$4"
  if [ -f "$dest" ]; then
    local actual_sha
    actual_sha="$(shasum -a 256 "$dest" | awk '{print $1}')"
    if [ "$actual_sha" = "$expected_sha" ]; then
      echo "[download] $label already present and verified ($(du -h "$dest" | cut -f1))"
      return 0
    fi
    echo "[download] $label present but sha256 MISMATCH; re-downloading"
    rm -f "$dest"
  fi
  echo "[download] fetching $label -> $dest"
  curl -L --fail -C - --retry 5 --retry-delay 5 -o "$dest" "$url"
  local rc=$?
  if [ $rc -ne 0 ]; then
    echo "[download] FAILED to fetch $label (curl rc=$rc)" >&2
    exit 1
  fi
  local actual_sha
  actual_sha="$(shasum -a 256 "$dest" | awk '{print $1}')"
  if [ "$actual_sha" != "$expected_sha" ]; then
    echo "[download] sha256 VERIFICATION FAILED for $label: got $actual_sha" >&2
    exit 1
  fi
  echo "[download] $label verified ($(du -h "$dest" | cut -f1))"
}

run_cfg() {
  local name="$1"; shift 1
  echo "=== [$name] starting server $(date '+%F %T') extra_args: $*" >> "$LOG"
  "$BIN" -m "$MODEL" --mmproj "$MMPROJ" --port $PORT --host 127.0.0.1 -c 8192 -ngl 0 \
    --jinja --no-webui --no-warmup -b 2048 -ub 2048 -fa on -ctk q8_0 -ctv q8_0 "$@" \
    > "$OUT_DIR/qwen_server_$name.log" 2>&1 &
  local pid=$!
  local up=0 i=0
  for i in $(seq 1 240); do
    sleep 5
    if [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/health)" = "200" ]; then up=1; break; fi
    kill -0 $pid 2>/dev/null || break
  done
  if [ $up -ne 1 ]; then
    echo "[$name] FAILED to become healthy (server log tail follows)" >> "$LOG"
    tail -5 "$OUT_DIR/qwen_server_$name.log" >> "$LOG"
    kill $pid 2>/dev/null
    echo "{\"config\": \"$name\", \"error\": \"unhealthy\"}" >> "$RESULTS"
    return 1
  fi
  echo "[$name] healthy after ~$((i*5))s, rss_kb=$(ps -o rss= -p $pid | tr -d ' ')" >> "$LOG"
  curl -s --max-time 3600 http://127.0.0.1:$PORT/completion -H 'Content-Type: application/json' \
    -d "{\"prompt\": \"$PROMPT\", \"n_predict\": $NPRED, \"temperature\": 0}" > "$OUT_DIR/qwen_resp_$name.json"
  python3 - "$name" "$OUT_DIR/qwen_resp_$name.json" "$NPRED" >> "$RESULTS" <<'EOF'
import json, sys
name, path, npred = sys.argv[1], sys.argv[2], int(sys.argv[3])
try:
    d = json.load(open(path))
    t = d.get('timings', {})
    keep = {k: t.get(k) for k in ('prompt_n','prompt_ms','prompt_per_second','predicted_n','predicted_ms','predicted_per_second','draft_n','draft_n_accepted')}
    keep['config'] = name
    keep['n_predict_requested'] = npred
    print(json.dumps(keep))
except Exception as e:
    print(json.dumps({'config': name, 'error': str(e)}))
EOF
  echo "[$name] done $(date '+%T'), rss_kb_after=$(ps -o rss= -p $pid | tr -d ' ')" >> "$LOG"
  # The "engine.log shows it in use" evidence: capture any draft/MTP lines.
  echo "[$name] draft/MTP log lines:" >> "$LOG"
  grep -iE "draft|mtp|spec" "$OUT_DIR/qwen_server_$name.log" | tail -10 >> "$LOG"
  kill $pid 2>/dev/null
  wait $pid 2>/dev/null
  sleep 5
}

echo "==== qwen3.6-27b MTP bench started $(date '+%F %T') ====" > "$LOG"
echo "binary: $("$BIN" --version 2>&1 | head -1)" >> "$LOG"
echo "machine: $(sw_vers -productName) $(sw_vers -productVersion), $(sysctl -n machdep.cpu.brand_string 2>/dev/null || sysctl -n hw.model), $(sysctl -n hw.memsize | awk '{print $1/1073741824 " GB RAM"}')" >> "$LOG"
echo "reps per config: $REPS, n_predict: $NPRED" >> "$LOG"

download_and_verify "$MODEL_URL" "$MODEL" "$MODEL_SHA256" "Qwen3.6-27B-Q4_K_M.gguf"
download_and_verify "$MMPROJ_URL" "$MMPROJ" "$MMPROJ_SHA256" "mmproj-BF16.gguf"

: > "$RESULTS"

# Interleave the configs so a machine-frequency drift shows up as spread
# within a config rather than a systematic ordering bias.
for rep in $(seq 1 "$REPS"); do
  run_cfg "base_r${rep}"
  run_cfg "mtp2_r${rep}" --spec-type draft-mtp --spec-draft-n-max 2
  run_cfg "mtp3_r${rep}" --spec-type draft-mtp --spec-draft-n-max 3
  run_cfg "mtp4_r${rep}" --spec-type draft-mtp --spec-draft-n-max 4
done

echo "==== ALL DONE $(date '+%F %T') ====" >> "$LOG"
python3 - "$RESULTS" <<'EOF' | tee -a "$LOG"
import json, statistics, sys
rows = []
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    try:
        rows.append(json.loads(line))
    except Exception:
        continue
groups = {}
for r in rows:
    if 'predicted_per_second' in r and r['predicted_per_second'] is not None:
        groups.setdefault(r['config'].rsplit('_', 1)[0], []).append(r['predicted_per_second'])
print("==== summary (tok/s, mean +/- sd over reps) ====")
for cfg in sorted(groups):
    vals = groups[cfg]
    sd = statistics.stdev(vals) if len(vals) > 1 else 0.0
    print(f"{cfg}: {statistics.mean(vals):.4f} +/- {sd:.4f}  (n={len(vals)}: {', '.join(f'{v:.4f}' for v in vals)})")
EOF
