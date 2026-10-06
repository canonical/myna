#!/bin/bash
# Run Myna Settings inside an e2e VM under its own Xvfb (x11, 1100x800) on
# the autologin user's real session bus, drive it through AT-SPI
# (shot-driver.py) and leave screenshots and logs in ./out. run-suite.sh
# pushes this directory and the binary to ~/myna-shot and runs it as the
# user (suites/lib.sh `shot`).
#
# Usage: shot.sh [--polkit allow|deny|cancel] [--monitor NAME] [step ...]
#   --polkit MODE   answer snapd's and the pkexec --apply-plan and --set-up
#                   prompts with a temporary rule; cancel answers through
#                   cancel-agent.py, like pressing Cancel
#   --monitor NAME  log processes, snapd changes and the journal to
#                   out/NAME-{monitor,journal}.log
# Steps: see shot-driver.py.
set -uo pipefail
cd "$(dirname "$0")" || exit 2
mode=none monitor=
while [ $# -gt 0 ]; do
    case $1 in
        --polkit) mode=$2; shift 2 ;;
        --monitor) monitor=$2; shift 2 ;;
        *) break ;;
    esac
done
case $mode in none|allow|deny|cancel) ;; *) echo "shot.sh: bad --polkit $mode" >&2; exit 2 ;; esac
rule=/etc/polkit-1/rules.d/49-myna-shot.rules
monitor_pids=()
mkdir -p out
glib-compile-schemas schemas/

cleanup() {
    sudo rm -f "$rule"
    if [ ${#monitor_pids[@]} -gt 0 ]; then
        # A root executor outlives the app: keep logging until it is gone.
        for _ in $(seq 450); do pgrep -u root -x myna-config >/dev/null || break; sleep 2; done
        sleep 4
        sudo kill "${monitor_pids[@]}" 2>/dev/null
    fi
}
trap cleanup EXIT

if [ "$mode" != none ]; then
    case $mode in allow) result=YES ;; deny) result=NO ;; cancel) result=AUTH_ADMIN ;; esac
    # A binary outside /usr/bin carries no Myna action, so pkexec asks for
    # org.freedesktop.policykit.exec: answered for this binary only.
    sudo tee "$rule" >/dev/null <<RULES
polkit.addRule(function(action, subject) {
    if (subject.user != "$USER") return polkit.Result.NOT_HANDLED;
    if (action.id.indexOf("io.snapcraft.snapd.") == 0 ||
        action.id == "com.canonical.Myna.Config.apply-plan" ||
        action.id == "com.canonical.Myna.Config.set-up" ||
        (action.id == "org.freedesktop.policykit.exec" &&
         action.lookup("program") == "$PWD/myna-config"))
        return polkit.Result.$result;
    return polkit.Result.NOT_HANDLED;
});
RULES
    sleep 1  # polkitd reloads rules on inotify
fi

if [ -n "$monitor" ]; then
    ( while :; do
        echo "=== $(date +%T)"
        pgrep -af "pkexec|myna-config --(apply|set-up)|snap (run|install|remove|set)" | cut -c1-200
        snap changes 2>&1 | tail -3
        sleep 2
      done > out/"$monitor"-monitor.log 2>&1 ) &
    monitor_pids=($!)
    # shellcheck disable=SC2024 # the log belongs to the user
    sudo journalctl -f -n 0 -o short-precise > out/"$monitor"-journal.log 2>&1 &
    monitor_pids+=($!)
fi

export MODE=$mode
xvfb-run -a -s "-screen 0 1100x800x24" bash -s -- "$@" <<'INNER'
export GDK_BACKEND=x11 GSETTINGS_SCHEMA_DIR=$PWD/schemas
# Under x11 GTK loads libim-ibus, which on newer GTK + ibus (no IBus
# reachable from Xvfb) recurses until the stack overflows as soon as a
# window with a text widget maps. Users on Wayland never load it.
export GTK_IM_MODULE=gtk-im-context-simple
unset WAYLAND_DISPLAY
# Without a compositor GTK paints popover and dialog shadows opaque black.
xcompmgr -n 2>/dev/null & comp=$!
./myna-config >> out/app.log 2>&1 &
pid=$!
[ "$MODE" = cancel ] && { python3 cancel-agent.py $pid >> out/agent.log 2>&1 & agent=$!; }
wid=$(timeout 30 xdotool search --sync --onlyvisible --pid $pid | head -1)
[ -n "$wid" ] || { echo "shot.sh: no window" >&2; cat out/app.log >&2; kill $pid; exit 4; }
xdotool windowmove "$wid" 0 0 windowsize "$wid" 1100 800
sleep 1
OUT_DIR=$PWD/out APP_PID=$pid python3 shot-driver.py "$@"
rc=$?
kill $pid $comp ${agent:-} 2>/dev/null; wait $pid 2>/dev/null
exit $rc
INNER
