#!/usr/bin/env bash
# snapshot: dictation
# myna: store
# requires: E2E_UPGRADE_CHANNEL
# A user's upgrade to the next release: the store's myna and backends, as the
# last release left them on edge, refreshed to E2E_UPGRADE_CHANNEL (a branch
# snaps.yml released a candidate to, e.g. latest/edge/rc1) in one change, as
# auto-refresh does, while the HUD runs. A sideload cannot stand in: it keeps
# no store components, gets no store-granted connections and skips the
# refresh-mode of the revision it replaces.
#
#   1. with the HUD up (hosted by myna-hud-host on Xubuntu and Plasma, by the
#      extension Stonking's gnome-shell-ubuntu-extensions ships; Noble's and
#      Resolute's GNOME have none in the VM), the refresh goes ahead once the revision it starts from marks the HUD
#      refresh-mode: ignore-running (issue #38); from an older one the HUD is
#      stopped first, as its users wait for logout or the forced refresh
#   2. every store snap tracks the channel at a new revision with as many
#      components as before, myna:backend stays on the fake backend, and the
#      daemon, the backends' servers and the HUD run the new revisions
#   3. a dictation lands in a field through the upgraded daemon
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
CHANNEL=${E2E_UPGRADE_CHANNEL:?}
# snap info names the track: latest/edge/rc1, never edge/rc1.
case $CHANNEL in latest/*) ;; *) CHANNEL=latest/$CHANNEL ;; esac
FIRST="The quick brown fox"
FULL="The quick brown fox jumps over the lazy dog."
LIB='. myna-shot/dictation-lib.sh'

dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}
# NAME REVISION COMPONENTS, one line per store snap (the fake is sideloaded).
store_snaps() {
    on "snap list | awk 'NR > 1 && \$1 ~ /^myna/ && \$3 !~ /^x/ {
        c = \"-\"; if (match(\$6, /components\\[[0-9]+/)) c = substr(\$6, RSTART + 11, RLENGTH - 11)
        print \$1, \$3, c }'"
}

on 'mkdir -p myna-shot/out'
lxc file push --uid 1000 --gid 1000 "$REPO/tests/spread/adapter-smoke/fixture.wav" "$VM/home/ubuntu/myna-shot/speech.wav"

assert_on "precondition: the backend is the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_on "precondition: myna is the store's" \
    "snap list myna | awk 'NR == 2 && \$3 !~ /^x/' | grep -q ."
assert_on "precondition: the daemon is idle" "$LIB; wait_until 20 dictation_state_is idle"
BEFORE=$(store_snaps)
printf 'before:\n%s\n' "$BEFORE"
SNAPS=$(awk '{print $1}' <<<"$BEFORE" | tr '\n' ' ')

echo "-- 1. the refresh, with the HUD up"
[ "$DESKTOP" = gnome ] || dict 'hud_host_start' >/dev/null
# Up means one HUD that stays: a revision that lacks the host the desktop
# needs (layer-shell came after 0.2.4) exits at once and is respawned, which
# snapd never sees as running.
HUD=false
# shellcheck disable=SC2016 # expands in the VM
on 'p=$(pgrep -nu "$USER" -x myna-hud) && sleep 2 && [ "$(pgrep -nu "$USER" -x myna-hud)" = "$p" ]' \
    >/dev/null && HUD=true
case $DESKTOP in
    xubuntu) expect=true ;;
    kde) expect=false; on 'snap run myna.hud --help | grep -q layer-shell' >/dev/null && expect=true ;;
    gnome) expect=$HUD ;; # only Stonking's packaged extension hosts one
esac
if [ "$HUD" = "$expect" ]; then
    echo "PASS: the HUD runs as the installed revision can ($HUD)"
else
    echo "FAIL: the HUD runs as the installed revision can ($HUD, expected $expect)"; FAILED=1
fi
# The revision being replaced decides whether snapd counts the HUD as busy.
ignored=false
on "python3 -c 'import sys, yaml
hud = yaml.safe_load(open(\"/snap/myna/current/meta/snap.yaml\"))[\"apps\"][\"hud\"]
sys.exit(hud.get(\"refresh-mode\") != \"ignore-running\")'" && ignored=true
if $HUD && ! $ignored; then
    echo "NOTE: the installed myna predates the HUD's ignore-running (#38); stopping the HUD first"
    # shellcheck disable=SC2016 # expands in the VM
    on 'systemctl --user stop snap.myna.myna.service
pkill -x myna-hud-host || true
for _ in $(seq 40); do pgrep -x myna-hud >/dev/null || exit 0; pkill -x myna-hud; sleep 0.5; done
exit 1' || { echo "the HUD did not stop" >&2; exit 1; }
fi
for s in $SNAPS; do
    on "sudo snap switch --channel=$CHANNEL $s" >/dev/null || { echo "could not switch $s" >&2; exit 1; }
done
if on "sudo snap refresh $SNAPS"; then
    echo "PASS: one refresh of $SNAPS goes ahead"
else
    echo "FAIL: one refresh of $SNAPS goes ahead"; FAILED=1
    on 'snap tasks --last=refresh' || true
fi
# The stopped host comes back as it would at the next login.
if $HUD && ! $ignored && [ "$DESKTOP" != gnome ]; then
    dict 'hud_host_start' >/dev/null || true
fi

echo "-- 2. the snaps after the refresh"
AFTER=$(store_snaps)
printf 'after:\n%s\n' "$AFTER"
# on reads stdin (lxc exec), so the loop reads the list from another descriptor.
while read -r name rev comps <&3; do
    new_rev='' new_comps=''
    read -r _ new_rev new_comps < <(grep "^$name " <<<"$AFTER")
    if [ -n "${new_rev:-}" ] && [ "$new_rev" != "$rev" ]; then
        echo "PASS: $name moved to a new revision ($rev -> $new_rev)"
    else
        echo "FAIL: $name moved to a new revision ($rev -> ${new_rev:-gone})"; FAILED=1
    fi
    if [ "$new_comps" = "$comps" ]; then
        echo "PASS: $name keeps its $comps components"
    else
        echo "FAIL: $name keeps its $comps components (now ${new_comps:-none})"; FAILED=1
    fi
    assert_on "$name tracks $CHANNEL" "snap info $name | grep -qx 'tracking: *$CHANNEL'"
done 3<<<"$BEFORE"
NEW=$(awk '$1 == "myna" {print $2}' <<<"$AFTER")
assert_on "myna:backend is still the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_soon "the daemon runs again" 'systemctl --user is-active snap.myna.myna.service'
for s in $SNAPS; do
    [ "$s" = myna ] && continue
    assert_soon "$s's server runs" "systemctl is-active snap.$s.server.service"
done
# The refreshed servers load their models right after the refresh, which on a
# runner's few cores starves the capture thread. The dictation goes to the
# fake, so they have nothing left to show.
for s in $SNAPS; do
    [ "$s" = myna ] || on "sudo snap stop $s" >/dev/null
done
if $HUD; then
    assert_dict "the HUD comes back from the new revision" \
        "wait_until 30 sh -c 'pgrep -u \"\$USER\" -ax myna-hud | grep -q \"/snap/myna/$NEW/\"'"
fi

echo "-- 3. a dictation through the upgraded daemon"
assert_on "the virtual microphone is made the default source" "$LIB; mic_make_default"
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
# GNOME starts in the Overview, where a new window is not focused.
dict 'key escape'
assert_dict "the field app is up" 'field_start'
assert_dict "the field is focused and the window active" 'field_focus plain'
assert_dict "the field starts empty" 'field_clear'
dict dictation_toggle
assert_dict "recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands while recording" "wait_until 10 field_is plain '$FIRST'"
dict dictation_toggle
assert_dict "idle after the stop" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
assert_on "the daemon reached the backend through the new revision" \
    "journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'backend: connected to /var/snap/myna/$NEW/'"
dict "journalctl --user -u snap.myna.myna.service --no-pager -o short-precise > myna-shot/out/daemon.log" || true
on 'snap changes > myna-shot/out/snap-changes.txt; snap tasks --last=refresh > myna-shot/out/refresh-tasks.txt' || true
suite_status
