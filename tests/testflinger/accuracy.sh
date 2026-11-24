#!/bin/bash
# The accuracy half of the Testflinger nightly, run as root from a checkout on
# the lab machine, everything from latest/edge/main, which snaps.yml publishes
# from main each night: the adapter smoke through the Myna client, then the
# FLEURS smoke tier through each backend, gated per language. Results land in
# out/.
set -eux
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$HERE/out"
cd "$HERE/out"

# What this machine is, so a latency change can be told from a hardware one.
{
    echo "arch: $(dpkg --print-architecture)"
    grep -m1 PRETTY_NAME /etc/os-release
    echo "kernel: $(uname -r)"
    lscpu | grep -m1 'Model name' || true
    echo "cpus: $(nproc)"
    lspci 2>/dev/null | grep -Ei 'vga|3d|display' || true
    nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>&1 || true
} > machine.out
cat machine.out

# A noprovision machine keeps the last run's snaps and their state.
for s in $(snap list 2>/dev/null | awk '$1 ~ /^myna/ {print $1}'); do
    snap remove --purge "$s"
done

# `myna-bench download-corpus --preset fleurs-smoke`, published once: 27 MB
# instead of the 4.3 GB of per-language archives it is cut from.
TIER=https://github.com/canonical/myna/releases/download/v0.3.3/fleurs-smoke.tar.gz
TIER_SHA256=c95c566d9e36ec9f98b61a88b8b8c6dda72fd32cad5910db58f870fa913f7a7b
curl -fsSL "$TIER" -o fleurs-smoke.tar.gz
echo "$TIER_SHA256  fleurs-smoke.tar.gz" | sha256sum -c
tar -xzf fleurs-smoke.tar.gz

snap set system experimental.user-daemons=true
snap install --channel=latest/edge/main myna
snap install --channel=latest/edge/main myna-whisper+model-tiny
SNAP_UNDER_TEST=myna-whisper MODELCTL=myna-whisper.whisper MODEL=tiny \
    "$HERE/../spread/adapter-smoke/smoke.sh"

# The bench reinstalls every target itself; the smoke's server would
# otherwise sit resident through the sweep.
snap remove --purge myna-whisper

# Lab images boot on powersave, which the bench refuses: its numbers are
# not comparable. The machine is throwaway, so nothing is restored.
for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
    [ -e "$g" ] || continue
    echo performance > "$g" || true
done
{
    echo "driver: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_driver 2>&1)"
    echo "available: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors 2>&1)"
    grep -H . /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor 2>&1
} > cpufreq.out
cat cpufreq.out
if grep -H . /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor 2>/dev/null | grep -qv ':performance$'; then
    echo "cannot set the performance governor on this machine, see cpufreq.out" >&2
    exit 1
fi

apt-get install -y -q python3-venv
"$HERE/../../dev/build-bench.sh"
BENCH="python3 $HERE/../../myna-bench.pyz"
$BENCH run --config "$HERE/fleurs.yaml"
$BENCH summarize --in fleurs.jsonl --no-ci --by-category --gate "$HERE/ceilings.yaml"

# Last, so the window is the whole run: the machine was deployed for it.
journalctl -b --no-pager > journal.log
python3 -I "$HERE/../lib/apparmor_denials.py" journal.log > apparmor.out || { cat apparmor.out; exit 1; }
cat apparmor.out
