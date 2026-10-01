#!/bin/bash
# The VM half of shot.sh, fed on stdin to bash on the VM (over ssh).
# Arguments: release dir polkit-mode polkit-actions apply-mode monitor language real fake steps...
#
# Ported from ~/myna-onboarding-stable/tools/shot-remote.sh with the
# live-desktop guards removed: the target is always a disposable VM, so
# --real and realshot: are allowed everywhere and no d-bus proxy is needed.
set -uo pipefail
rel=$1 dir=$2 mode=$3 actions=$4 apply_mode=$5 monitor=$6 language=$7 real=$8 fake=$9; shift 9
cd "$dir" || exit 2
rule=/etc/polkit-1/rules.d/49-myna-shot.rules
apply_rule=/etc/polkit-1/rules.d/49-myna-shot-apply.rules
monitor_pids=

if pgrep -x myna-config >/dev/null; then
    echo "shot.sh: a myna-config is already running on $rel; close it first" >&2
    exit 3
fi
glib-compile-schemas schemas/
store=~/snap/myna/common/.config/glib-2.0/settings/keyfile
[ -e seed-keyfile ] && { mkdir -p "$(dirname "$store")"; cp seed-keyfile "$store"; }

cleanup() {
    sudo rm -f "$rule" "$apply_rule"
    if [ -n "$monitor_pids" ]; then
        # A root executor outlives the app: keep logging until it is gone.
        for _ in $(seq 450); do pgrep -u root -x myna-config >/dev/null || break; sleep 2; done
        sleep 4
        sudo kill $monitor_pids 2>/dev/null
    fi
}
trap cleanup EXIT

answer() {
    case $1 in
        allow) spawn= result=YES ;;
        deny) spawn= result=NO ;;
        cancel|slow-cancel|long-cancel) spawn= result=AUTH_ADMIN ;;
        slow-allow) spawn='polkit.spawn(["/bin/sleep", "6"]);' result=YES ;;
        slow-deny) spawn='polkit.spawn(["/bin/sleep", "6"]);' result=NO ;;
    esac
}

if [ "$mode" != none ]; then
    answer "$mode"
    ids=$(printf '"io.snapcraft.snapd.%s",' ${actions//,/ })
    sudo tee "$rule" >/dev/null <<EOF
polkit.addRule(function(action, subject) {
    if ([${ids%,}].indexOf(action.id) >= 0 && subject.user == "$USER") {
        $spawn
        return polkit.Result.$result;
    }
});
EOF
    sleep 1  # polkitd reloads rules on inotify
fi

if [ "$apply_mode" != none ]; then
    answer "$apply_mode"
    # A binary outside /usr/bin carries no Myna action, so pkexec asks for
    # org.freedesktop.policykit.exec: allowed for these two programs only.
    sudo tee "$apply_rule" >/dev/null <<EOF
polkit.addRule(function(action, subject) {
    if (subject.user != "$USER") return polkit.Result.NOT_HANDLED;
    if (action.id == "com.canonical.Myna.Config.apply-plan" ||
        (action.id == "org.freedesktop.policykit.exec" &&
         (action.lookup("program") == "$dir/myna-config" ||
          action.lookup("program") == "/usr/bin/myna-config"))) {
        $spawn
        return polkit.Result.$result;
    }
    return polkit.Result.NOT_HANDLED;
});
EOF
    sleep 1
fi

if [ -n "$monitor" ]; then
    ( while :; do
        echo "=== $(date +%T)"
        ps -eo pid,ppid,etimes,user,args | grep -E "[p]kexec|[m]yna-config --apply|[s]nap (run|install|remove)" | cut -c1-200
        python3 snapd-rest.py changes 2>&1 | cut -c1-300
        sleep 2
      done > out/"$monitor"-monitor.log 2>&1 ) &
    monitor_pids="$!"
    sudo journalctl -f -n 0 -o short-precise > out/"$monitor"-journal.log 2>&1 &
    monitor_pids="$monitor_pids $!"
fi

# An agent for the app that dismisses every prompt: a polkit cancel.
agent_mode=
case $mode in cancel) agent_mode=0 ;; slow-cancel) agent_mode=6 ;; long-cancel) agent_mode=45 ;; esac
[ "$apply_mode" = cancel ] && agent_mode=0

export XDG_RUNTIME_DIR=/run/user/$(id -u)
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus
export AGENT_MODE=$agent_mode
[ -z "$language" ] || export LANGUAGE=$language
export SHOT_HOST=$rel

# --real: the VM's own Wayland session, so portal dialogs and the wizard
# share one display and stacking can be shot.
if [ "$real" = 1 ]; then
    export WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland GSETTINGS_SCHEMA_DIR=$PWD/schemas
    unset DISPLAY
    # The session's own desktop variables, so what the app launches (GNOME
    # Settings from Change shortcut) finds snap desktop files as a user's would.
    while IFS= read -r line; do
        case $line in XDG_DATA_DIRS=*|XDG_CURRENT_DESKTOP=*|XDG_SESSION_TYPE=*|XDG_SESSION_DESKTOP=*|DESKTOP_SESSION=*) export "$line" ;; esac
    done < <(systemctl --user show-environment)
    ./myna-config > app.log 2>&1 &
    pid=$!
    [ -z "$AGENT_MODE" ] || { python3 cancel-agent.py $pid $AGENT_MODE > agent.log 2>&1 & agent=$!; }
    sleep 3
    OUT_DIR=$PWD/out APP_PID=$pid python3 shot-driver.py "$@"
    rc=$?
    kill $pid 2>/dev/null; wait $pid 2>/dev/null
    [ -n "${agent:-}" ] && kill $agent 2>/dev/null
    [ -s app.log ] && { echo "--- myna-config log" >&2; cat app.log >&2; }
    exit $rc
fi
export FAKE=$fake REAL_BUS=$DBUS_SESSION_BUS_ADDRESS
# Never the seat display: unset what the caller's session exported.
unset DISPLAY WAYLAND_DISPLAY
xvfb-run -a -s "-screen 0 1100x800x24" bash -s -- "$@" <<'INNER'
export GDK_BACKEND=x11 GSETTINGS_SCHEMA_DIR=$PWD/schemas
# Under x11 GTK loads libim-ibus, which on newer GTK + ibus (no IBus
# reachable from Xvfb) recurses until the stack overflows as soon as a
# window with a text widget maps. Users on Wayland never load it.
export GTK_IM_MODULE=gtk-im-context-simple
unset WAYLAND_DISPLAY
case $DISPLAY in :0|:1) echo "shot.sh: refusing to run on display $DISPLAY" >&2; exit 5 ;; esac
# --fake-daemon: a private bus for the app and the driver, where nothing is
# activatable and fake-daemon.py is the dictation daemon. Both keep the real
# session's accessibility bus: a second one cannot start its registry.
if [ "$FAKE" = 1 ]; then
    export AT_SPI_BUS_ADDRESS=$(DBUS_SESSION_BUS_ADDRESS=$REAL_BUS gdbus call --session \
        -d org.a11y.Bus -o /org/a11y/bus -m org.a11y.Bus.GetAddress | sed "s/^('//; s/',)\$//")
    mkdir -p fakebus
    cat > fakebus/bus.conf <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
EOF
    DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --config-file=fakebus/bus.conf --fork --print-address=1 --print-pid=3 3>fakebus/pid)
    export DBUS_SESSION_BUS_ADDRESS APP_BUS=$DBUS_SESSION_BUS_ADDRESS
    rm -f fake-bind
    python3 fake-daemon.py > out/fake-daemon.log 2>&1 & fake_pid=$!
    sleep 1
fi
# Without a compositor GTK paints popover and dialog shadows opaque black.
xcompmgr -n 2>/dev/null & comp=$!
DBUS_SESSION_BUS_ADDRESS=${APP_BUS:-$DBUS_SESSION_BUS_ADDRESS} ./myna-config > app.log 2>&1 &
pid=$!
[ -z "$AGENT_MODE" ] || { python3 cancel-agent.py $pid $AGENT_MODE > agent.log 2>&1 & agent=$!; }
wid=$(timeout 30 xdotool search --sync --onlyvisible --pid $pid | head -1)
[ -n "$wid" ] || { echo "shot.sh: no window" >&2; cat app.log >&2; kill $pid; exit 4; }
# "natural" as the first step keeps the window's own default size.
if [ "${1:-}" = natural ]; then shift; xdotool windowmove "$wid" 0 0
else xdotool windowmove "$wid" 0 0 windowsize "$wid" 1100 800; fi
sleep 1
OUT_DIR=$PWD/out APP_PID=$pid python3 shot-driver.py "$@"
rc=$?
kill $pid 2>/dev/null; wait $pid 2>/dev/null
kill $comp 2>/dev/null; [ -n "${agent:-}" ] && kill $agent 2>/dev/null
[ -n "${fake_pid:-}" ] && { kill $fake_pid; kill $(cat fakebus/pid); } 2>/dev/null
[ -s app.log ] && { echo "--- myna-config log" >&2; cat app.log >&2; }
exit $rc
INNER
rc=$?
exit $rc
