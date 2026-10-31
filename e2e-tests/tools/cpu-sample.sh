#!/usr/bin/env bash
# cpu-sample.sh FILE: until killed, one line a second of how the machine's
# CPUs spent it. run-suite.sh runs it in the VM and on the host for every
# suite, so lost capture can be laid against time a hypervisor took from
# the vCPUs (steal), which no guest-side priority can win back.
#
#   time      wall clock, as the journal's short-precise
#   busy%     user + system time, of all CPUs
#   steal%    time the hypervisor ran something else on our vCPUs
#   psi-ms    CPU pressure: ms some runnable task waited for a CPU
#   load1     the 1-minute load average
#
# A second past 200 ms of pressure adds a line naming the five busiest
# processes over the next 0.2 s. A line `data-loops:` lists every PipeWire
# data-loop thread (pid/comm, class, priority) whenever that set changes,
# so a capture thread that never ran realtime shows.
set -u
exec >>"$1" 2>/dev/null
cpus=$(nproc)
stat() { read -r _ user nice system idle iowait irq softirq steal _ < /proc/stat; }
psi() { sed -n 's/^some .*total=//p' /proc/pressure/cpu 2>/dev/null || echo 0; }
loops() {
    ps -eL -o pid=,cls=,rtprio=,comm= | awk '$4 ~ /data-loop/ {
        f = "/proc/" $1 "/comm"; p = "?"; getline p < f; close(f)
        printf " %s/%s/%s=%s:%s", $1, p, $4, $2, $3 }'
}
p_loops=''
stat; p_busy=$((user + nice + system + irq + softirq)) p_steal=$steal
p_total=$((p_busy + idle + iowait + steal)) p_psi=$(psi)
echo "time busy% steal% psi-ms load1 ($cpus CPUs)"
while sleep 1; do
    stat; busy=$((user + nice + system + irq + softirq))
    total=$((busy + idle + iowait + steal)) now=$(psi)
    span=$((total - p_total)); [ "$span" -gt 0 ] || span=1
    echo "$(date +%T.%6N) $((100 * (busy - p_busy) / span)) $((100 * (steal - p_steal) / span))" \
        "$(((now - p_psi) / 1000)) $(cut -d' ' -f1 /proc/loadavg)"
    if [ $(((now - p_psi) / 1000)) -gt 200 ]; then
        top -bn2 -d0.2 -w 200 | awk '/^top -/ { n++ } n == 2 && f { printf " %s=%s", $12, $9; if (++k == 5) exit }
            n == 2 && $1 == "PID" { f = 1 }' | sed 's/^/  busiest:/'; echo
    fi
    l=$(loops)
    [ "$l" = "$p_loops" ] || echo "  data-loops:${l:- none}"
    p_loops=$l
    p_busy=$busy p_steal=$steal p_total=$total p_psi=$now
done
