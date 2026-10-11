#!/bin/bash
# Round 3: download gpt-oss-20b, bench it (-ngl 0 baseline, -ub PP, then the
# Track C Metal NO_RESIDENCY experiment), finish with the riskiest run:
# NO_RESIDENCY on the >RAM glm-4.5-air, guarded by a wired-memory monitor.
set -u
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LLAMA_BIN:-$REPO_ROOT/crates/app/resources/llama/llama-server}"
MODELS_DIR="${OPENPAL_MODELS_DIR:-$HOME/Library/Application Support/com.myopenpal.openpal/models}"
GPTOSS="$MODELS_DIR/gpt-oss-20b-mxfp4.gguf"
GLM="$MODELS_DIR/GLM-4.5-Air-UD-IQ2_XXS.gguf"
PORT=18080
OUT_DIR="${BENCH_OUT_DIR:-/tmp/openpal-bench}"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/round3_bench.log"
RESULTS="$OUT_DIR/round3_bench_results.jsonl"
# Abort the Metal experiment if wired memory exceeds this (bytes): 13.5 GB on
# a 16 GB machine means the experiment failed its premise (pages got wired).
WIRED_ABORT_BYTES=$((13500 * 1024 * 1024))

echo "==== round3 started $(date '+%F %T') ====" > "$LOG"
: > "$RESULTS"

# --- Download gpt-oss-20b if missing, verify sha256 ---
if [ ! -f "$GPTOSS" ]; then
  echo "downloading gpt-oss-20b $(date '+%T')" >> "$LOG"
  curl -sL --fail -o "$GPTOSS.part" "https://huggingface.co/ggml-org/gpt-oss-20b-GGUF/resolve/main/gpt-oss-20b-mxfp4.gguf" >> "$LOG" 2>&1 || { echo "download FAILED" >> "$LOG"; exit 1; }
  mv "$GPTOSS.part" "$GPTOSS"
fi
SIZE=$(stat -f%z "$GPTOSS")
echo "gpt-oss-20b size: $SIZE (expect 12109566560)" >> "$LOG"
echo "verifying sha256… $(date '+%T')" >> "$LOG"
SHA=$(shasum -a 256 "$GPTOSS" | cut -d' ' -f1)
echo "sha256: $SHA (expect be37a636aca0fc1aae0d32325f82f6b4d21495f06823b5fbc1898ae0303e9935)" >> "$LOG"
if [ "$SHA" != "be37a636aca0fc1aae0d32325f82f6b4d21495f06823b5fbc1898ae0303e9935" ]; then
  echo "SHA MISMATCH — aborting" >> "$LOG"; exit 1
fi

wired_bytes() {
  vm_stat | awk '/Pages wired down/ {gsub(/\./,"",$4); print $4 * 16384}'
}

# run_cfg <name> <model> <npred> <use_metal_no_residency:0|1> [extra args...]
run_cfg() {
  local name="$1"; local model="$2"; local npred="$3"; local nores="$4"; shift 4
  echo "=== [$name] starting $(date '+%F %T') nores=$nores extra: $*" >> "$LOG"
  if [ "$nores" = "1" ]; then
    GGML_METAL_NO_RESIDENCY=1 "$BIN" -m "$model" --port $PORT --host 127.0.0.1 -c 8192 --jinja --no-webui --no-warmup "$@" > "$OUT_DIR/server_$name.log" 2>&1 &
  else
    "$BIN" -m "$model" --port $PORT --host 127.0.0.1 -c 8192 --jinja --no-webui --no-warmup "$@" > "$OUT_DIR/server_$name.log" 2>&1 &
  fi
  local pid=$!

  # Wired-memory watchdog: kills the server if the experiment's premise
  # (nothing gets wired) fails. Runs for the whole config.
  (
    while kill -0 $pid 2>/dev/null; do
      w=$(wired_bytes)
      echo "[$name] wired_bytes=$w rss_kb=$(ps -o rss= -p $pid | tr -d ' ')" >> "$OUT_DIR/monitor_$name.log"
      if [ "$w" -gt "$WIRED_ABORT_BYTES" ]; then
        echo "[$name] WIRED ABORT: $w > $WIRED_ABORT_BYTES — killing server" >> "$LOG"
        kill -9 $pid 2>/dev/null
        break
      fi
      sleep 10
    done
  ) &
  local monpid=$!

  local up=0 i=0
  for i in $(seq 1 240); do
    sleep 5
    if [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/health)" = "200" ]; then up=1; break; fi
    kill -0 $pid 2>/dev/null || break
  done
  if [ $up -ne 1 ]; then
    echo "[$name] FAILED to become healthy (server log tail follows)" >> "$LOG"
    tail -8 "$OUT_DIR/server_$name.log" >> "$LOG"
    kill $pid 2>/dev/null; kill $monpid 2>/dev/null
    echo "{\"config\": \"$name\", \"error\": \"unhealthy\"}" >> "$RESULTS"
    sleep 5
    return 1
  fi
  echo "[$name] healthy after ~$((i*5))s, wired=$(wired_bytes)" >> "$LOG"
  printf '%s' '{"prompt": "Explain in plain language why the sky appears blue during the day and reddish at sunset, and mention one common misconception people have about this phenomenon.", "n_predict": NPRED, "temperature": 0}' | sed "s/NPRED/$npred/" > "$OUT_DIR/req_$name.json"
  curl -s --max-time 3600 http://127.0.0.1:$PORT/completion -H 'Content-Type: application/json' \
    -d @"$OUT_DIR/req_$name.json" > "$OUT_DIR/resp_$name.json"
  python3 - "$name" "$OUT_DIR/resp_$name.json" >> "$RESULTS" <<'EOF'
import json, sys
name, path = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
    t = d.get('timings', {})
    keep = {k: t.get(k) for k in ('prompt_n','prompt_ms','prompt_per_second','predicted_n','predicted_ms','predicted_per_second')}
    keep['config'] = name
    print(json.dumps(keep))
except Exception as e:
    print(json.dumps({'config': name, 'error': str(e)}))
EOF
  echo "[$name] done $(date '+%T'), wired_after=$(wired_bytes)" >> "$LOG"
  kill $pid 2>/dev/null
  wait $pid 2>/dev/null
  kill $monpid 2>/dev/null
  sleep 5
}

# gpt-oss-20b: DiskSlow-recipe baseline (near-fit regime), then Track C
run_cfg oss_ngl0 "$GPTOSS" 64 0 -ngl 0
run_cfg oss_nores99 "$GPTOSS" 64 1 -ngl 99 --cpu-moe
# glm-4.5-air: the >RAM Metal experiment — riskiest, last.
run_cfg glm_nores99 "$GLM" 32 1 -ngl 99 --cpu-moe
echo "==== ALL DONE $(date '+%F %T') ====" >> "$LOG"
