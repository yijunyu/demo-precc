#!/usr/bin/env bash
# gen_kernel_i_files.sh — preprocess a stratified sample of Linux kernel .c files into .i files
#
# Usage:
#   ./scripts/gen_kernel_i_files.sh [--kernel-dir <dir>] [--out-dir <dir>] [--count <N>] [--jobs <J>]
#
# Subsystems sampled (stratified, proportional weights):
#   kernel/, mm/, fs/ext4/, drivers/net/, drivers/block/, drivers/char/,
#   drivers/gpu/drm/i915/, sound/core/, crypto/, arch/x86/kernel/, security/, init/, ipc/

set -euo pipefail

KDIR="${KERNEL_DIR:-/home/y00577373/linux-6.14}"
OUTDIR="${OUT_DIR:-/home/y00577373/precc/tests/kernel}"
COUNT="${SAMPLE_COUNT:-200}"
JOBS="${PREPROCESS_JOBS:-$(nproc)}"
SEED="${RANDOM_SEED:-42}"

while [[ $# -gt 0 ]]; do
  case $1 in
    --kernel-dir) KDIR="$2"; shift 2 ;;
    --out-dir)    OUTDIR="$2"; shift 2 ;;
    --count)      COUNT="$2"; shift 2 ;;
    --jobs)       JOBS="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

mkdir -p "$OUTDIR"

# ── Subsystem weights (percent of total COUNT) ────────────────────────────────
declare -a SUBSYSTEMS=( kernel mm fs/ext4 drivers/net drivers/block drivers/char
                        drivers/gpu/drm/i915 sound/core crypto arch/x86/kernel
                        security init ipc )
declare -a WEIGHTS=(    10     10  8       12          7             7
                        8                  8     8      8
                        7        4    3 )

# ── Collect stratified sample ─────────────────────────────────────────────────
echo "=== Collecting kernel source files from $KDIR ==="

JOBLIST=$(mktemp /tmp/kernel_joblist.XXXXXX)

for idx in "${!SUBSYSTEMS[@]}"; do
  subsys="${SUBSYSTEMS[$idx]}"
  frac="${WEIGHTS[$idx]}"
  n_files=$(( (COUNT * frac + 50) / 100 ))
  [[ $n_files -lt 1 ]] && n_files=1

  subsys_dir="$KDIR/$subsys"
  [[ -d "$subsys_dir" ]] || { echo "  SKIP (no dir): $subsys"; continue; }

  mapfile -t candidates < <(
    find "$subsys_dir" -name "*.c" \
      ! -path "*/selftests/*" ! -path "*/testcases/*" ! -path "*/tools/*" \
      ! -name "*.mod.c" \
      -printf "%p\n" 2>/dev/null | sort
  )

  total=${#candidates[@]}
  [[ $total -eq 0 ]] && { echo "  SKIP (no .c): $subsys"; continue; }

  seed_val=$(( SEED + ${#subsys} * 31 ))
  mapfile -t selected < <(
    printf '%s\n' "${candidates[@]}" | \
    awk -v seed="$seed_val" -v n="$n_files" \
      'BEGIN{srand(seed)} {lines[NR]=$0} END{
         for(i=NR;i>1;i--){j=int(rand()*(i))+1; t=lines[i];lines[i]=lines[j];lines[j]=t}
         for(k=1;k<=n&&k<=NR;k++) print lines[k]
       }'
  )

  echo "  $subsys: ${#selected[@]} of $total files"

  for src in "${selected[@]}"; do
    subsys_slug="${subsys//\//__}"
    base=$(basename "$src" .c)
    out="$OUTDIR/${subsys_slug}__${base}.i"
    echo "$subsys|$src|$out"
  done >> "$JOBLIST"
done

total_jobs=$(wc -l < "$JOBLIST")
echo ""
echo "=== Preprocessing $total_jobs files with -j$JOBS ==="

# ── Build a worker script ─────────────────────────────────────────────────────
WORKER=$(mktemp /tmp/kernel_worker.XXXXXX.sh)
cat > "$WORKER" << 'WORKER_EOF'
#!/usr/bin/env bash
set -euo pipefail
KDIR="$1"
entry="$2"

subsys="${entry%%|*}"
rest="${entry#*|}"
src="${rest%%|*}"
out="${rest#*|}"
base=$(basename "$src" .c)

srcdir=$(dirname "$src")
if gcc -E \
  -I"$srcdir" \
  -I"$KDIR/include" \
  -I"$KDIR/arch/x86/include" \
  -I"$KDIR/arch/x86/include/generated" \
  -I"$KDIR/include/uapi" \
  -I"$KDIR/arch/x86/include/uapi" \
  -I"$KDIR/arch/x86/include/generated/uapi" \
  -I"$KDIR/include/generated/uapi" \
  -D__KERNEL__ \
  -DKBUILD_MODFILE="\"$base\"" \
  -DKBUILD_BASENAME="\"$base\"" \
  -DKBUILD_MODNAME="\"$base\"" \
  -include "$KDIR/include/linux/kconfig.h" \
  -Wno-unused-value \
  "$src" -o "$out" 2>/dev/null; then
  lines=$(wc -l < "$out")
  bytes=$(stat -c%s "$out")
  echo -e "$subsys\t$src\t$out\t$lines\t$bytes\tok"
else
  echo -e "$subsys\t$src\t$out\t0\t0\tfail"
fi
WORKER_EOF
chmod +x "$WORKER"

# ── Run in parallel ───────────────────────────────────────────────────────────
MANIFEST="$OUTDIR/manifest.tsv"
echo -e "subsystem\tsource_path\ti_file\tlines\tbytes\tok" > "$MANIFEST"

cat "$JOBLIST" | xargs -P"$JOBS" -I{} bash "$WORKER" "$KDIR" {} >> "$MANIFEST"

rm -f "$JOBLIST" "$WORKER"

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "=== Results ==="
ok_count=$(awk -F'\t' 'NR>1 && $6=="ok"' "$MANIFEST" | wc -l)
fail_count=$(awk -F'\t' 'NR>1 && $6=="fail"' "$MANIFEST" | wc -l)
total_lines=$(awk -F'\t' 'NR>1 && $6=="ok" {sum+=$4} END{print sum+0}' "$MANIFEST")
total_mb=$(awk -F'\t' 'NR>1 && $6=="ok" {sum+=$5} END{printf "%.1f", sum/1024/1024}' "$MANIFEST")

echo "  OK:    $ok_count files"
echo "  FAIL:  $fail_count files"
echo "  Lines: $total_lines total, $(awk -F'\t' "NR>1 && \$6==\"ok\"" "$MANIFEST" | wc -l) files → avg $(( total_lines / (ok_count+1) )) lines/file"
echo "  Size:  ${total_mb} MB"
echo ""
echo "  Manifest: $MANIFEST"
echo "  .i files: $OUTDIR/*.i"
echo ""

echo "=== Per-subsystem stats ==="
awk -F'\t' 'NR>1 && $6=="ok" {
  cnt[$1]++; lines[$1]+=$4
} END {
  for (s in cnt) printf "  %-35s %4d files  avg %d lines\n", s, cnt[s], lines[s]/cnt[s]
}' "$MANIFEST" | sort -k2 -rn

echo ""
echo "=== Line count distribution ==="
awk -F'\t' 'NR>1 && $6=="ok" {print $4}' "$MANIFEST" | sort -n | awk '
  {
    n++; total+=$1
    if ($1 < 1000)   b1++
    else if ($1 < 10000)  b2++
    else if ($1 < 50000)  b3++
    else if ($1 < 100000) b4++
    else                  b5++
  }
  END {
    printf "  0-1K lines:      %d files\n",   b1+0
    printf "  1K-10K lines:    %d files\n",   b2+0
    printf "  10K-50K lines:   %d files\n",   b3+0
    printf "  50K-100K lines:  %d files\n",   b4+0
    printf "  100K+ lines:     %d files\n",   b5+0
    mean = (n > 0) ? total/n : 0
    printf "  Mean:            %d lines\n",   mean
  }
'
