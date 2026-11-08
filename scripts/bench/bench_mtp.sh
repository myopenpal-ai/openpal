#!/bin/bash
# MTP self-draft A/B on GLM-4.5-Air, exact DiskSlow recipe (-ngl 0 -c 8192 --no-warmup)
set -u
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LLAMA_BIN:-$REPO_ROOT/crates/app/resources/llama/llama-server}"
MODELS_DIR="${OPENPAL_MODELS_DIR:-$HOME/Library/Application Support/com.myopenpal.openpal/models}"
MODEL="${OPENPAL_MODEL:-$MODELS_DIR/GLM-4.5-Air-UD-IQ2_XXS.gguf}"
PORT=18080
OUT_DIR="${BENCH_OUT_DIR:-/tmp/openpal-bench}"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/mtp_bench.log"
RESULTS="$OUT_DIR/mtp_bench_results.jsonl"
PROMPT="Explain in plain language why the sky appears blue during the day and reddish at sunset, and mention one common misconception people have about this phenomenon."

run_cfg() {
  local name="$1"; local npred="$2"; shift 2
  echo "=== [$name] starting server $(date '+%F %T') extra_args: $*" >> "$LOG"
  "$BIN" -m "$MODEL" --port $PORT --host 127.0.0.1 -c 8192 -ngl 0 --jinja --no-webui --no-warmup "$@" > "$OUT_DIR/server_$name.log" 2>&1 &
  local pid=$!
  local up=0 i=0
  for i in $(seq 1 240); do
    sleep 5
    if [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/health)" = "200" ]; then up=1; break; fi
    kill -0 $pid 2>/dev/null || break
  done
  if [ $up -ne 1 ]; then
    echo "[$name] FAILED to become healthy (server log tail follows)" >> "$LOG"
    tail -5 "$OUT_DIR/server_$name.log" >> "$LOG"
    kill $pid 2>/dev/null
    echo "{\"config\": \"$name\", \"error\": \"unhealthy\"}" >> "$RESULTS"
    return 1
  fi
  echo "[$name] healthy after ~$((i*5))s, rss_kb=$(ps -o rss= -p $pid | tr -d ' ')" >> "$LOG"
  curl -s --max-time 2400 http://127.0.0.1:$PORT/completion -H 'Content-Type: application/json' \
    -d "{\"prompt\": \"$PROMPT\", \"n_predict\": $npred, \"temperature\": 0}" > "$OUT_DIR/resp_$name.json"
  python3 - "$name" "$OUT_DIR/resp_$name.json" >> "$RESULTS" <<'EOF'
import json, sys
name, path = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
    t = d.get('timings', {})
    keep = {k: t.get(k) for k in ('prompt_n','prompt_ms','prompt_per_second','predicted_n','predicted_ms','predicted_per_second','draft_n','draft_n_accepted')}
    keep['config'] = name
    print(json.dumps(keep))
except Exception as e:
    print(json.dumps({'config': name, 'error': str(e)}))
EOF
  echo "[$name] done $(date '+%T'), rss_kb_after=$(ps -o rss= -p $pid | tr -d ' ')" >> "$LOG"
  kill $pid 2>/dev/null
  wait $pid 2>/dev/null
  sleep 5
}

echo "==== MTP bench started $(date '+%F %T') ====" > "$LOG"
: > "$RESULTS"
run_cfg base64 64
run_cfg mtp2 64 --spec-type draft-mtp --spec-draft-n-max 2
run_cfg mtp3 64 --spec-type draft-mtp --spec-draft-n-max 3
run_cfg base32 32
echo "==== ALL DONE $(date '+%F %T') ====" >> "$LOG"
