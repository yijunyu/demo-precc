#!/usr/bin/env bash
# run the remaining experiments (PCH j* sweep already done as ids 24-29)
set -euo pipefail

PRECC=/home/y00577373/.cargo/bin/precc
SQLITE3_I=/home/y00577373/precc/tests/sqlite3/sqlite3.i
VIM_DIR=/home/y00577373/precc/tests/vim/src
GCC=${CC:-gcc}
NCPUS=$(nproc)
TMPLOG=$(mktemp /tmp/precc_exp.XXXXXX)
trap "rm -f $TMPLOG" EXIT

time_cmd() {
  local n=$1; shift
  local times=()
  for _ in $(seq 1 "$n"); do
    local t
    t=$( { time "$@" > /dev/null 2>&1; } 2>&1 | grep real | awk '{print $2}' | \
         sed 's/m/:/;s/s//' | awk -F: '{printf "%.3f", $1*60+$2}' )
    times+=("$t")
  done
  printf '%s\n' "${times[@]}" | sort -n | awk "NR==int($n/2)+1"
}

parse_timings() {
  local log=$1
  ctags_t=$(grep "ctags processing" "$log" | tail -1 | grep -oP '[0-9]+\.[0-9]+' || echo "")
  dep_t=$(grep "dependency computation" "$log" | tail -1 | grep -oP '[0-9]+\.[0-9]+' || echo "")
}

rec() { "$PRECC" record "$@"; }

# ── sqlite3 baselines ─────────────────────────────────────────────────────────
echo "=== sqlite3 baselines ==="
SQLITE3_BASELINE_J1=$(time_cmd 3 $GCC -O2 -c "$SQLITE3_I" -o /dev/null)
echo "  gcc j1 baseline: ${SQLITE3_BASELINE_J1}s"

SRC_FRAC=0.963; FN_BRACES=11388
FILE_SIZE=$(stat -c%s "$SQLITE3_I")
FILE_LINES=$(wc -l < "$SQLITE3_I")

# ── sqlite3: split jobs sweep ────────────────────────────────────────────────
# NOTE: sqlite3 split mode generates 11K individual .pu.c files and compiles
# each with a separate gcc invocation — ~160s regardless of job count due to
# subprocess-launch overhead. Only j1 and j48 recorded (endpoints sufficient).
echo "=== sqlite3 split jobs sweep (j1 + j48 only — endpoints) ==="
for jobs in 1 48; do
  echo "  split j$jobs..."
  t=$(time_cmd 1 bash -c "SPLIT=1 JOBS=$jobs '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  rec sqlite3 sqlite3.i split \
    --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
    --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
    --split --jobs "$jobs" --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
    ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
    --notes "sqlite3 split j$jobs (11K .pu.c files, subprocess overhead dominates)"
done

# ── sqlite3: passthrough_threshold sweep (PCH mode) ──────────────────────────
echo "=== sqlite3 passthrough_threshold sweep ==="
for thresh in 0 10 50 100 200 500 11000 99999; do
  echo "  pch thresh=$thresh..."
  t=$(time_cmd 3 bash -c "PRECC_PCH=1 SPLIT=1 PASSTHROUGH_THRESHOLD=$thresh '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  effective="pch"
  [ "$thresh" -gt 11388 ] && effective="passthrough"
  rec sqlite3 sqlite3.i "$effective" \
    --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
    --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
    --pch --split --jobs "$NCPUS" --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
    ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
    --notes "sqlite3 PCH PASSTHROUGH_THRESHOLD=$thresh (effective=$effective)"
done

# ── sqlite3: pch_min_src_frac sweep ──────────────────────────────────────────
echo "=== sqlite3 pch_min_src_frac sweep ==="
for min_frac in 0.0 0.3 0.5 0.7 0.95 0.99; do
  echo "  pch min_src_frac=$min_frac..."
  t=$(time_cmd 3 bash -c "PRECC_PCH=1 SPLIT=1 PRECC_PCH_MIN_SRC_FRAC=$min_frac '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  effective="pch"
  python3 -c "exit(0 if $SRC_FRAC >= $min_frac else 1)" 2>/dev/null || effective="split"
  rec sqlite3 sqlite3.i "$effective" \
    --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
    --src-frac $SRC_FRAC --fn-braces $FN_BRACES \
    --pch --split --jobs "$NCPUS" --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
    ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
    --notes "sqlite3 PRECC_PCH_MIN_SRC_FRAC=$min_frac (effective=$effective)"
done

# ── sqlite3: passthrough (no split) ──────────────────────────────────────────
echo "=== sqlite3 passthrough ==="
t=$(time_cmd 3 bash -c "PASSTHROUGH_THRESHOLD=99999 '$PRECC' '$SQLITE3_I' > /dev/null 2>'$TMPLOG'")
parse_timings "$TMPLOG"
rec sqlite3 sqlite3.i passthrough \
  --precc-time "$t" --baseline-time "$SQLITE3_BASELINE_J1" \
  --src-frac $SRC_FRAC --fn-braces $FN_BRACES --jobs "$NCPUS" \
  --file-size "$FILE_SIZE" --lines "$FILE_LINES" \
  ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
  --notes "sqlite3 passthrough (PASSTHROUGH_THRESHOLD=99999)"

# ── vim per-file sweep ────────────────────────────────────────────────────────
echo ""
echo "=== vim per-file experiments ==="
VIM_BASELINE_J1_TOTAL=0

for ifile in "$VIM_DIR"/*.i; do
  fname=$(basename "$ifile")
  fsize=$(stat -c%s "$ifile"); flines=$(wc -l < "$ifile")
  cfg="$VIM_DIR/.precc-config.toml"
  src_frac=$(grep -A5 "path = \"$fname\"" "$cfg" | grep src_frac | head -1 | grep -oP '[0-9]+\.[0-9]+' || echo "0.1")
  fn_braces=$(grep -A5 "path = \"$fname\"" "$cfg" | grep fn_braces | head -1 | grep -oP '[0-9]+' || echo "100")
  baseline=$(time_cmd 3 $GCC -O2 -g -c "$ifile" -o /dev/null 2>/dev/null)
  echo "  $fname  baseline=${baseline}s src_frac=$src_frac fn_braces=$fn_braces"

  # split, various jobs
  for jobs in 1 8 24 48; do
    t=$(time_cmd 3 bash -c "SPLIT=1 JOBS=$jobs '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    rec vim "$fname" split \
      --precc-time "$t" --baseline-time "$baseline" \
      --src-frac "$src_frac" --fn-braces "$fn_braces" \
      --split --jobs "$jobs" --file-size "$fsize" --lines "$flines" \
      ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
      --notes "vim $fname split j$jobs"
  done

  # PCH (auto-disables for header-dominated files)
  for jobs in 8 48; do
    t=$(time_cmd 3 bash -c "PRECC_PCH=1 SPLIT=1 JOBS=$jobs '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    grep -q "PCH: wrote" "$TMPLOG" 2>/dev/null && eff="pch" || eff="split"
    rec vim "$fname" "$eff" \
      --precc-time "$t" --baseline-time "$baseline" \
      --src-frac "$src_frac" --fn-braces "$fn_braces" \
      --pch --split --jobs "$jobs" --file-size "$fsize" --lines "$flines" \
      ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
      --notes "vim $fname PRECC_PCH=1 j$jobs (effective=$eff)"
  done

  # PCH with lowered min_src_frac (force PCH even on header-heavy files)
  for min_frac in 0.0 0.1 0.2; do
    t=$(time_cmd 3 bash -c "PRECC_PCH=1 SPLIT=1 PRECC_PCH_MIN_SRC_FRAC=$min_frac JOBS=48 '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
    parse_timings "$TMPLOG"
    grep -q "PCH: wrote" "$TMPLOG" 2>/dev/null && eff="pch" || eff="split"
    rec vim "$fname" "$eff" \
      --precc-time "$t" --baseline-time "$baseline" \
      --src-frac "$src_frac" --fn-braces "$fn_braces" \
      --pch --split --jobs 48 --file-size "$fsize" --lines "$flines" \
      ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
      --notes "vim $fname PCH_MIN_SRC_FRAC=$min_frac j48 (effective=$eff)"
  done

  # passthrough
  t=$(time_cmd 3 bash -c "PASSTHROUGH_THRESHOLD=99999 '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  rec vim "$fname" passthrough \
    --precc-time "$t" --baseline-time "$baseline" \
    --src-frac "$src_frac" --fn-braces "$fn_braces" --jobs "$NCPUS" \
    --file-size "$fsize" --lines "$flines" \
    ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
    --notes "vim $fname passthrough"

  # with vs without config file
  t=$(time_cmd 3 bash -c "SPLIT=1 JOBS=48 '$PRECC' '$ifile' > /dev/null 2>'$TMPLOG'")
  parse_timings "$TMPLOG"
  rec vim "$fname" split \
    --precc-time "$t" --baseline-time "$baseline" \
    --src-frac "$src_frac" --fn-braces "$fn_braces" \
    --split --jobs 48 --file-size "$fsize" --lines "$flines" \
    --used-config \
    ${ctags_t:+--ctags-time "$ctags_t"} ${dep_t:+--dep-time "$dep_t"} \
    --notes "vim $fname split j48 WITH .precc-config.toml (config hit)"
done

echo ""
echo "=== Summary ==="
"$PRECC" db --summary
echo ""
"$PRECC" db --query "SELECT id, project, filename, strategy, jobs, round(src_frac,3), fn_braces, round(precc_time_s,3), round(speedup,2), precc_version, git_rev FROM experiments WHERE id > 23 ORDER BY id" | head -80
