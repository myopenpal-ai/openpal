#!/bin/bash
# Round 2 on GLM-4.5-Air: ngram-mod TG, -ub PP A/B (1024-token prompt), KV-quant check
set -u
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LLAMA_BIN:-$REPO_ROOT/crates/app/resources/llama/llama-server}"
MODELS_DIR="${OPENPAL_MODELS_DIR:-$HOME/Library/Application Support/com.myopenpal.openpal/models}"
MODEL="${OPENPAL_MODEL:-$MODELS_DIR/GLM-4.5-Air-UD-IQ2_XXS.gguf}"
PORT=18080
OUT_DIR="${BENCH_OUT_DIR:-/tmp/openpal-bench}"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/round2_bench.log"
RESULTS="$OUT_DIR/round2_bench_results.jsonl"

SHORT_PROMPT="Explain in plain language why the sky appears blue during the day and reddish at sunset, and mention one common misconception people have about this phenomenon."
# ~1024-token prompt: repeated distinct sentences to avoid trivial n-gram
# structure while staying deterministic. Built inline with python.
LONG_PROMPT_FILE="$OUT_DIR/long_prompt.txt"
python3 - "$LONG_PROMPT_FILE" <<'EOF'
import sys
sents = []
for i in range(90):
    sents.append(f"Fact {i}: the {['river','mountain','forest','desert','glacier','valley','island','plateau','canyon','delta'][i%10]} region number {i} recorded {(i*37)%100} millimeters of rain and {(i*13)%40} degrees in month {i%12 + 1}.")
text = " ".join(sents) + " Question: summarize the overall climate pattern across all regions in two sentences."
open(sys.argv[1], "w").write(text)
EOF

run_cfg() {
  local name="$1"; local npred="$2"; local prompt_file="$3"; shift 3
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
  python3 - "$prompt_file" "$npred" > "$OUT_DIR/req_$name.json" <<'EOF'
import json, sys
prompt = open(sys.argv[1]).read()
print(json.dumps({"prompt": prompt, "n_predict": int(sys.argv[2]), "temperature": 0}))
EOF
  curl -s --max-time 5400 http://127.0.0.1:$PORT/completion -H 'Content-Type: application/json' \
    -d @"$OUT_DIR/req_$name.json" > "$OUT_DIR/resp_$name.json"
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

SHORT_FILE="$OUT_DIR/short_prompt.txt"
printf '%s' "$SHORT_PROMPT" > "$SHORT_FILE"

echo "==== round2 bench started $(date '+%F %T') ====" > "$LOG"
: > "$RESULTS"
# 1. ngram-mod TG (zero-asset speculative mode; same short prompt as baseline)
run_cfg ngram64 64 "$SHORT_FILE" --spec-type ngram-mod
# 2. PP A/B on the ~1k-token prompt: default -ub 512 vs -ub 2048
run_cfg ppub512 16 "$LONG_PROMPT_FILE"
run_cfg ppub2048 16 "$LONG_PROMPT_FILE" -b 2048 -ub 2048
# 3. KV quant + flash-attn no-regression TG check
run_cfg kvq32 32 "$SHORT_FILE" -fa on -ctk q8_0 -ctv q8_0
echo "==== ALL DONE $(date '+%F %T') ====" >> "$LOG"
