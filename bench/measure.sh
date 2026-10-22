#!/usr/bin/env bash
# Measure an RDP client's resource use over the benchmark workload, from the Mac — no camera.
# Reports network throughput, CPU and memory for scry or the Microsoft Windows App, so the two
# can be compared on the same Cloud PC and network.
#
# Usage:  bench/measure.sh [process-name] [seconds]
#   process-name  "scry" (default) or "Windows App"
#   seconds       measurement window (default 35, ~ the 30s workload + margin)
#
# Run the workload (bench/workload.html) on the Cloud PC, then start this as the IDLE phase
# begins. Do it once per client and compare the two summaries.
set -euo pipefail

NAME="${1:-scry}"
SECS="${2:-35}"
CSV="$(dirname "$0")/results.csv"

pids() { pgrep -x "$NAME" || true; }

if [[ -z "$(pids)" ]]; then
  echo "No process named '$NAME' is running."
  echo "Connect with that client first (for scry, open the viewer)."
  exit 1
fi

# Sum cumulative bytes_in/bytes_out (from nettop) over every process whose name matches,
# so networking helper processes are included too. nettop rows are "name.pid,in,out,".
net_bytes() {
  nettop -P -x -L 1 -J bytes_in,bytes_out 2>/dev/null \
    | awk -F, -v n="$NAME" '
        index($1, n ".") == 1 { bin += $2; bout += $3 }
        END { printf "%d %d", bin+0, bout+0 }'
}

# Sum CPU seconds used so far (ps TIME, formatted [HH:]MM:SS[.cc]) across all PIDs.
cpu_secs() {
  ps -o time= -p "$(pids | paste -sd, -)" 2>/dev/null | awk '
    { gsub(/^ +/, ""); n = split($1, a, ":"); s = 0
      for (i = 1; i <= n; i++) s = s * 60 + a[i]
      total += s } END { printf "%.2f", total+0 }'
}

# Sum resident memory (KB) across all PIDs.
rss_kb() {
  ps -o rss= -p "$(pids | paste -sd, -)" 2>/dev/null | awk '{ t += $1 } END { print t+0 }'
}

echo "Measuring '$NAME' (pids: $(pids | paste -sd' ' -)) for ${SECS}s…"
echo "Start the workload now if you haven't."
echo

read -r NIN0 NOUT0 <<<"$(net_bytes)"
CPU0="$(cpu_secs)"
T0="$(date +%s)"
PEAK_RSS=0

# Poll memory every 2s for the peak; CPU/network use start/end deltas.
while (( $(date +%s) - T0 < SECS )); do
  r="$(rss_kb)"; (( r > PEAK_RSS )) && PEAK_RSS="$r"
  sleep 2
done

read -r NIN1 NOUT1 <<<"$(net_bytes)"
CPU1="$(cpu_secs)"
T1="$(date +%s)"
ELAPSED=$(( T1 - T0 ))

awk -v name="$NAME" -v secs="$ELAPSED" \
    -v nin="$(( NIN1 - NIN0 ))" -v nout="$(( NOUT1 - NOUT0 ))" \
    -v cpu0="$CPU0" -v cpu1="$CPU1" -v peak="$PEAK_RSS" -v csv="$CSV" '
BEGIN {
  mb_in = nin / 1e6; mb_out = nout / 1e6
  kbps_in = nin * 8 / secs / 1e3; kbps_out = nout * 8 / secs / 1e3
  cpu_pct = (cpu1 - cpu0) / secs * 100
  peak_mb = peak / 1024
  printf "\n──────── %s ── %ds ────────\n", name, secs
  printf "Download : %8.2f MB   (%7.0f kbit/s)\n", mb_in, kbps_in
  printf "Upload   : %8.2f MB   (%7.0f kbit/s)\n", mb_out, kbps_out
  printf "CPU      : %8.1f %% average (across cores)\n", cpu_pct
  printf "Memory   : %8.0f MB peak\n", peak_mb
  printf "─────────────────────────────────────\n"
  new = (system("test -f " csv) != 0)
  if (new) printf "when,client,seconds,down_mb,up_mb,down_kbps,cpu_pct,peak_mb\n" >> csv
  cmd = "date +%FT%T"; cmd | getline ts; close(cmd)
  printf "%s,%s,%d,%.2f,%.2f,%.0f,%.1f,%.0f\n", ts, name, secs, mb_in, mb_out, kbps_in, cpu_pct, peak_mb >> csv
}'
echo "Appended to $CSV"
