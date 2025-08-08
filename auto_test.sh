#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

sudo podman network rm criu-e2e-network -f

# Catch Ctrl+C to stop the whole loop cleanly
trap 'echo -e "\n[!] Caught interrupt. Stopping tests."; exit 130' INT

# Usage: ./repeat_test.sh [runs] [command...]
# Example: ./repeat_test.sh 50 sudo make test-e2e
RUNS="${1:-100}"
shift || true
CMD=("$@")
if [ ${#CMD[@]} -eq 0 ]; then
  CMD=(make test-e2e)
fi

STAMP="$(date +%Y%m%d-%H%M%S)"
OUTDIR="/tmp/criu-coordinator-e2e-test-$STAMP"
mkdir -p "$OUTDIR"

passes=0
fails=0
durations=()

# Colors
if [ -t 1 ]; then
  GREEN="\033[0;32m"; RED="\033[0;31m"; YELLOW="\033[1;33m"; NC="\033[0m"
else
  GREEN=""; RED=""; YELLOW=""; NC=""
fi

echo -e "${YELLOW}Running: ${CMD[*]}${NC}"
echo -e "${YELLOW}Iterations: $RUNS${NC}"
echo "Logs directory: $OUTDIR"
echo

for ((i=1; i<=RUNS; i++)); do
  start_ns=$(date +%s%N)
  log="$OUTDIR/run_${i}.log"

  if "${CMD[@]}" &> "$log"; then
    rc=0
  else
    rc=$?
  fi

  end_ns=$(date +%s%N)
  dur_ms=$(( (end_ns - start_ns)/1000000 ))
  durations+=("$dur_ms")

  if [ $rc -eq 0 ]; then
    passes=$((passes+1))
    if [ "${KEEP_ALL_LOGS:-0}" != "1" ]; then
      rm -f "$log"
    fi
    printf "[%3d/%d] %bPASS%b (%d ms)\n" "$i" "$RUNS" "$GREEN" "$NC" "$dur_ms"
  else
    fails=$((fails+1))
    printf "[%3d/%d] %bFAIL%b (%d ms) -> %s\n" "$i" "$RUNS" "$RED" "$NC" "$dur_ms" "$log"
    # Print the log content for failures
    echo -e "${RED}=== Log for run $i ===${NC}"
    cat "$log"
  fi

  # Sleep 5 seconds before next test unless it's the last one
  if [ $i -lt $RUNS ]; then
    sleep 5
  fi
done

# Summary
total=$((passes+fails))
pass_rate=$(awk -v p="$passes" -v t="$total" 'BEGIN{if(t>0) printf "%.2f", (p*100)/t; else print 0}')
fail_rate=$(awk -v f="$fails"  -v t="$total" 'BEGIN{if(t>0) printf "%.2f", (f*100)/t; else print 0}')

min=${durations[0]:-0}
max=${durations[0]:-0}
sum=0
for d in "${durations[@]:-0}"; do
  (( d < min )) && min=$d
  (( d > max )) && max=$d
  sum=$((sum + d))
done
avg=0
if [ "$total" -gt 0 ]; then
  avg=$(awk -v s="$sum" -v t="$total" 'BEGIN{printf "%.0f", s/t}')
fi

echo
echo "================ Summary ================"
printf "Total:   %d\n" "$total"
printf "Passed:  %d (%.2f%%)\n" "$passes" "$pass_rate"
printf "Failed:  %d (%.2f%%)\n" "$fails"  "$fail_rate"
printf "Timing:  avg=%d ms  min=%d ms  max=%d ms\n" "$avg" "$min" "$max"
echo "Logs:    $OUTDIR  (failures kept${KEEP_ALL_LOGS:+; successes too})"
echo "========================================"
