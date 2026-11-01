#!/usr/bin/env bash
# snapshot: bare
# The whole first-run onboarding on a bare machine: Welcome -> Install all
# components (snapd flag, Dictation app, model; polkit allowed) -> How to
# dictate -> Done closes the app. Afterwards the machine must be exactly
# what onboarding promises: flag on, snaps installed, backend connected,
# daemon active, shortcut set. On Xfce and Plasma the myna-config deb has
# just been installed, so the indicator's host is not running yet: the run
# starts it (issue #79).
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

P=onboarding-$REL
S=shots/onboarding-full

install_hud_host_entry

shot --polkit allow --monitor "$P" \
    "waitfor:Next@$BTN" shot:$S/01-welcome.png \
    "click:Next@$BTN" "waitfor:Install all components" shot:$S/02-components.png \
    "click:Install all components" "gone:Install all components" shot:$S/03-installing.png \
    "burst:$P-install:5:900:How to dictate" \
    "waitfor:Done@$BTN" shot:$S/04-how-to-dictate.png \
    "click:Done@$BTN" \
    "$(poll '! pgrep -x myna-config')"
ran=$?
# Where an install's time went, task by task: a download, a snapd refresh.
# shellcheck disable=SC2016 # expands in the VM
on 'for id in $(snap changes | awk "/Install/ {print \$1}"); do
    echo "== change $id"; snap tasks "$id"
done > myna-shot/out/install-tasks.txt 2>&1
# A seeded blob snapd installed from is linked into place: 3+ links, not 2.
echo "== seeded blobs (links, size, sha3)" >> myna-shot/out/install-tasks.txt
stat -c "%h %s %n" /var/cache/e2e-blobs/* >> myna-shot/out/install-tasks.txt 2>&1' || true
[ "$ran" = 0 ] || { echo "onboarding run failed" >&2; exit 1; }

assert_on "user-daemons flag on" \
    'sudo snap get system experimental.user-daemons | grep -q true'
assert_on "myna snap installed" \
    'snap list myna >/dev/null'
assert_on "myna-parakeet installed" \
    'snap list myna-parakeet >/dev/null'
assert_on "myna:backend connected to a provider" \
    'snap connections myna | grep -E "myna:backend[[:space:]]+myna-[a-z]+:provider" | grep -q .'
assert_on "dictation daemon active" \
    'systemctl --user is-active -q snap.myna.myna.service'
assert_shortcut_bound
assert_input_method_up
assert_hud_host_up
suite_status
