# Sourced by a script that runs ON the build host (remote). Usage: measure_run <label> <command...>
#
# Runs the command while sampling the host's CPU once a second, then prints how parallel it really was:
# busy cores (peak / average, with the host's idle background load subtracted), the share of time spent at
# various parallelism levels, and a 10 s timeline with the busiest processes. Settings like -j80 only say
# what is *allowed*; this says what happened. Returns the command's exit status.
measure_run() {
  local label=$1; shift
  local n; n=$(nproc)
  local f; f=$(mktemp)
  _cpu() { awk '/^cpu /{b=$2+$3+$4+$7+$8; print b, b+$5+$6}' /proc/stat; }

  # Baseline: what the machine is doing without us (other workloads on the host).
  local b0 t0 b1 t1 base
  read -r b0 t0 < <(_cpu); sleep 4; read -r b1 t1 < <(_cpu)
  base=$(awk -v b=$((b1 - b0)) -v t=$((t1 - t0)) -v n="$n" 'BEGIN{printf "%.2f", b/t*n}')

  (
    while :; do
      local sb st eb et top
      read -r sb st < <(_cpu)
      # second top iteration = the last ~1 s; three busiest commands as name:%cpu
      top=$(top -b -n 2 -d 1 -o %CPU -w 120 2>/dev/null |
        awk '/^top -/{blk++} blk==2 && $1 ~ /^[0-9]+$/ {printf "%s:%s ", $12, $9; if (++c==3) exit}')
      read -r eb et < <(_cpu)
      echo "$(date +%s.%N) $(awk -v b=$((eb - sb)) -v t=$((et - st)) -v n="$n" 'BEGIN{printf "%.2f", b/t*n}') ${top}" >> "$f"
    done
  ) &
  local sp=$!
  local start end rc
  start=$(date +%s.%N)
  "$@"; rc=$?
  end=$(date +%s.%N)
  kill "$sp" 2>/dev/null; wait "$sp" 2>/dev/null

  awk -v label="$label" -v n="$n" -v base="$base" -v start="$start" -v end="$end" '
    { ts=$1; raw=$2; net=raw-base; if (net<0) net=0
      k++; T[k]=ts; C[k]=net; S+=net; if (net>pk) pk=net
      if (net>=2) a2++; if (net>=8) a8++; if (net>=20) a20++; if (net>=40) a40++
      split($3, p, ":"); N[k]=p[1] }
    END {
      if (k==0) { print "[measure] no samples (build too short)"; exit }
      wall=end-start
      printf "\n[measure] %s: wall %.0f s on a %d-core host (background load %.1f cores subtracted)\n", label, wall, n, base
      printf "[measure]   busy cores: average %.1f, peak %.1f   (cpu-time ~ %.0f core-seconds)\n", S/k, pk, S/k*wall
      printf "[measure]   share of time with >=2 cores: %.0f%%, >=8: %.0f%%, >=20: %.0f%%, >=40: %.0f%%\n", 100*a2/k, 100*a8/k, 100*a20/k, 100*a40/k
      printf "[measure]   timeline (10 s windows: avg cores, busiest process):\n"
      w=-1
      for (i=1;i<=k;i++) { b=int((T[i]-T[1])/10)
        if (b!=w) { if (w>=0) flush(w); w=b; s=0; c=0; delete cnt }
        s+=C[i]; c++; cnt[N[i]]++ }
      flush(w)
    }
    function flush(w,   nm, best, bc) { best=""; bc=0; for (nm in cnt) if (cnt[nm]>bc) { bc=cnt[nm]; best=nm }
      printf "[measure]     %4d-%4ds  %5.1f cores  %s\n", w*10, w*10+10, s/c, best }
  ' "$f"
  rm -f "$f"
  return $rc
}
