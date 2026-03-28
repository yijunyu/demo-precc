#!/usr/bin/env bash
# run_experiments.sh — systematic experiment sweep across all precc config variants
#
# Records every timed run into ~/.precc/experiments.db with full provenance:
# project, strategy, jobs, pch, split_count, passthrough_threshold, precc_version, git_rev
#
# Usage:
#   ./scripts/run_experiments.sh [--sqlite3-only] [--vim-only] [--dry-run]

set -euo pipefail

PRECC=/home/y00577373/.cargo/bin/precc
SQLITE3_I=/home/y00577373/precc/tests/sqlite3/sqlite3.i
VIM_DIR=/home/y00577373/precc/tests/vim/src
GCC=${CC:-gcc}
RUNS=3        # timing reps for fast configs (< ~5s)
RUNS_SLOW=1   # timing reps for slow configs (split j1 on big files)

DRY_RUN=0
SQLITE3_ONLY=0
VIM_ONLY=0
for arg in "$@"; do
  case $arg in
    --dry-run)     DRY_RUN=1 ;;
    --sqlite3-only) SQLITE3_ONLY=1 ;;
    --vim-only)     VIM_ONLY=1 ;;
  esac
done

# ── helpers ──────────────────────────────────────────────────────────────────

# time_cmd <N> <cmd...>  → prints median wall time in seconds to stdout
time_cmd() {
  local n=$1; shift
  local times=()
  for _ in $(seq 1 "$n"); do
    local t
    t=$( { time "$@" > /dev/null 2>&1; } 2>&1 | grep real | awk '{print $2}' | \
         sed 's/m/:/;s/s//' | awk -F: '{printf "%.3f", $1*60+$2}' )
    times+=("$t")
  done
  # sort and take median
  printf '%s\n' "${times[@]}" | sort -n | awk "NR==int($n/2)+1"
}

# parse TIMING lines from precc stderr into associative-array-style output
parse_timings() {
  local log=$1
  ctags_t=$(grep "ctags processing" "$log" | tail -1 | grep -oP '[0-9]+\.[0-9]+' || echo "")
  dep_t=$(grep "dependency computation" "$log" | tail -1 | grep -oP '[0-9]+\.[0-9]+' || echo "")
  total_t=$(grep "TOTAL" "$log" | tail -1 | grep -oP '[0-9]+\.[0-9]+' || echo "")
}

record() {
  local project=$1; shift
  local filename=$1; shift
  local strategy=$1; shift
  # rest are flags passed directly to `precc record`
  if [ "$DRY_RUN" = "1" ]; then
    echo "[DRY] precc record $project $filename $strategy $*"
    return
  fi
  "$PRECC" record "$project" "$filename" "$strategy" "$@"
}

# ── baseline timings ─────────────────────────────────────────────────────────

echo "=== Measuring baselines ==="
NCPUS=$(nproc)

if [ "$VIM_ONLY" = "0" ] && [ -f "$SQLITE3_I" ]; then
  echo "  sqlite3 baseline (gcc single)..."
  SQLITE3_BASELINE_J1=$(time_cmd "$RUNS" $GCC -O2 -c "$SQLITE3_I" -o /dev/null)
  echo "  sqlite3 baseline (gcc -j1): ${SQLITE3_BASELINE_J1}s"
fi

if [ "$SQLITE3_ONLY" = "0" ] && [ -d "$VIM_DIR" ]; then
  VIM_FILES=("$VIM_DIR"/*.i)
  echo "  vim baseline (gcc -j${NCPUS})..."
  VIM_BASELINE_PAR=$(time_cmd "$RUNS" bash -c "ls '$VIM_DIR'/*.i | xargs -P$NCPUS -I{} $GCC -O2 -g -c {} -o /dev/null 2>/dev/null")
  echo "  vim baseline (gcc -j${NCPUS}): ${VIM_BASELINE_PAR}s"
fi

echo ""

# ── sqlite3 experiments ───────────────────────────────────────────────────────

if [ "$VIM_ONLY" = "0" ] && [ -f "$SQLITE3_I" ]; then
  echo "=== sqlite3 experiments ==="

  FILE_SIZE=$(stat -c%s "$SQLITE3_I" 2>/dev/null || stat -f%z "$SQLITE3_I")
  FILE_LINES=$(wc -l < "$SQLITE3_I")
  SRC_FRAC=0.963
  FN_BRACES=11388

  TMPLOG=$(mktemp /tmp/precc_exp.XXXXXX)

  # ── sweep: jobs (PCH mode, optimal strategy) ─────────────────────────────
  for jobs in 1 4 8 16 24 48; do
    echo "  sqlite3 pch j$jobs..."
    t=$(time_cmd "$RUNS" bash -c "PRECC_PCH=1 SPLIT=1 JOBS=$jobs '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    record sqlite3 sqlite3.i pch \
      --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
      --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
      --pch --split --jobs "$jobs" \
      --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
      ${ctags_t:+--ctags-time "$ctags_t"} \
      ${dep_t:+--dep-time "$dep_t"} \
      --notes "sqlite3 PCH sweep jobs=$jobs"
  done

  # ── sweep: jobs (split mode) ──────────────────────────────────────────────
  for jobs in 1 4 8 16 24 48; do
    echo "  sqlite3 split j$jobs..."
    # j1 is very slow (11K fns × gcc overhead); single run only
    reps=$([ "$jobs" -le 1 ] && echo "$RUNS_SLOW" || echo "$RUNS")
    t=$(time_cmd "$reps" bash -c "SPLIT=1 JOBS=$jobs '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    record sqlite3 sqlite3.i split \
      --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
      --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
      --split --jobs "$jobs" \
      --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
      ${ctags_t:+--ctags-time "$ctags_t"} \
      ${dep_t:+--dep-time "$dep_t"} \
      --notes "sqlite3 split sweep jobs=$jobs"
  done

  # ── sweep: passthrough_threshold with PCH ─────────────────────────────────
  for thresh in 0 10 50 100 200 500; do
    echo "  sqlite3 pch thresh=$thresh..."
    t=$(time_cmd "$RUNS" bash -c "PRECC_PCH=1 SPLIT=1 PASSTHROUGH_THRESHOLD=$thresh '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    record sqlite3 sqlite3.i pch \
      --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
      --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
      --pch --split --jobs "$NCPUS" \
      --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
      ${ctags_t:+--ctags-time "$ctags_t"} \
      ${dep_t:+--dep-time "$dep_t"} \
      --notes "sqlite3 PCH passthrough_threshold=$thresh"
  done

  # ── sweep: pch_min_src_frac ───────────────────────────────────────────────
  for min_frac in 0.0 0.3 0.5 0.7 0.9; do
    echo "  sqlite3 pch min_src_frac=$min_frac..."
    t=$(time_cmd "$RUNS" bash -c "PRECC_PCH=1 SPLIT=1 PRECC_PCH_MIN_SRC_FRAC=$min_frac '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    # determine effective strategy (pch disabled if src_frac < min_frac)
    effective="pch"
    if (( $(echo "$SRC_FRAC < $min_frac" | bc -l) )); then effective="split"; fi
    record sqlite3 sqlite3.i "$effective" \
      --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
      --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
      --pch --split --jobs "$NCPUS" \
      --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
      ${ctags_t:+--ctags-time "$ctags_t"} \
      ${dep_t:+--dep-time "$dep_t"} \
      --notes "sqlite3 pch_min_src_frac=$min_frac effective=$effective"
  done

  # ── passthrough (baseline comparison) ────────────────────────────────────
  echo "  sqlite3 passthrough..."
  t=$(time_cmd "$RUNS" bash -c "'$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  record sqlite3 sqlite3.i passthrough \
    --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
    --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
    --jobs "$NCPUS" \
    --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
    ${ctags_t:+--ctags-time "$ctags_t"} \
    ${dep_t:+--dep-time "$dep_t"} \
    --notes "sqlite3 passthrough (default, no split, no pch)"

  rm -f "$TMPLOG"
  echo ""
fi

# ── vim per-file experiments ──────────────────────────────────────────────────

if [ "$SQLITE3_ONLY" = "0" ] && [ -d "$VIM_DIR" ]; then
  echo "=== vim per-file experiments ==="

  TMPLOG=$(mktemp /tmp/precc_exp.XXXXXX)

  for ifile in "$VIM_DIR"/*.i; do
    fname=$(basename "$ifile")
    fsize=$(stat -c%s "$ifile" 2>/dev/null || stat -f%z "$ifile")
    flines=$(wc -l < "$ifile")

    # get metrics from profile config (already generated)
    cfg="$VIM_DIR/.precc-config.toml"
    if [ -f "$cfg" ]; then
      src_frac=$(grep -A5 "path = \"$fname\"" "$cfg" | grep "src_frac" | head -1 | grep -oP '[0-9]+\.[0-9]+' || echo "0.1")
      fn_braces=$(grep -A5 "path = \"$fname\"" "$cfg" | grep "fn_braces" | head -1 | grep -oP '[0-9]+' || echo "100")
    else
      src_frac=0.1; fn_braces=100
    fi

    # baseline: gcc single file
    baseline=$(time_cmd "$RUNS" $GCC -O2 -g -c "$ifile" -o /dev/null 2>/dev/null)
    echo "  vim/$fname baseline=${baseline}s src_frac=$src_frac fn_braces=$fn_braces"

    # ── split mode, various job counts ───────────────────────────────────────
    for jobs in 1 8 24 48; do
      t=$(time_cmd "$RUNS" bash -c "SPLIT=1 JOBS=$jobs '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
      parse_timings "$TMPLOG"
      record vim "$fname" split \
        --precc-time "$t" --baseline-time "$baseline" \
        --src-frac "$src_frac" --fn-braces "$fn_braces" \
        --split --jobs "$jobs" \
        --file-size "$fsize" --lines "$flines" \
        ${ctags_t:+--ctags-time "$ctags_t"} \
        ${dep_t:+--dep-time "$dep_t"} \
        --notes "vim $fname split j$jobs"
    done

    # ── PCH mode (should auto-disable for header-dominated files) ────────────
    for jobs in 8 48; do
      t=$(time_cmd "$RUNS" bash -c "PRECC_PCH=1 SPLIT=1 JOBS=$jobs '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
      parse_timings "$TMPLOG"
      # check if PCH was actually used (look for "PCH: wrote" in log)
      if grep -q "PCH: wrote" "$TMPLOG" 2>/dev/null; then
        eff_strategy="pch"
      else
        eff_strategy="split"
      fi
      record vim "$fname" "$eff_strategy" \
        --precc-time "$t" --baseline-time "$baseline" \
        --src-frac "$src_frac" --fn-braces "$fn_braces" \
        --pch --split --jobs "$jobs" \
        --file-size "$fsize" --lines "$flines" \
        ${ctags_t:+--ctags-time "$ctags_t"} \
        ${dep_t:+--dep-time "$dep_t"} \
        --notes "vim $fname PRECC_PCH=1 j$jobs (effective=$eff_strategy)"
    done

    # ── passthrough (default) ─────────────────────────────────────────────
    t=$(time_cmd "$RUNS" bash -c "'$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    record vim "$fname" passthrough \
      --precc-time "$t" --baseline-time "$baseline" \
      --src-frac "$src_frac" --fn-braces "$fn_braces" \
      --jobs "$NCPUS" \
      --file-size "$fsize" --lines "$flines" \
      ${ctags_t:+--ctags-time "$ctags_t"} \
      ${dep_t:+--dep-time "$dep_t"} \
      --notes "vim $fname passthrough (default)"

    # ── used-config mode ──────────────────────────────────────────────────
    if [ -f "$cfg" ]; then
      t=$(time_cmd "$RUNS" bash -c "SPLIT=1 JOBS=48 '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
      parse_timings "$TMPLOG"
      record vim "$fname" split \
        --precc-time "$t" --baseline-time "$baseline" \
        --src-frac "$src_frac" --fn-braces "$fn_braces" \
        --split --jobs 48 \
        --file-size "$fsize" --lines "$flines" \
        --used-config \
        ${ctags_t:+--ctags-time "$ctags_t"} \
        ${dep_t:+--dep-time "$dep_t"} \
        --notes "vim $fname split j48 WITH .precc-config.toml"
    fi
  done

  rm -f "$TMPLOG"
  echo ""
fi

# ── summary ───────────────────────────────────────────────────────────────────
echo "=== Results summary ==="
"$PRECC" db --summary
echo ""
echo "Full CSV: precc db --csv"
echo "DB path:  $("$PRECC" db --query 'SELECT 1' 2>&1 | grep -v "^1$" || echo ~/.precc/experiments.db)"
