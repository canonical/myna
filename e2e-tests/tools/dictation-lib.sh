# shellcheck shell=bash
# VM-side helpers for the dictation suite. Source inside the VM, as the
# autologin user: `. myna-shot/dictation-lib.sh`. run-suite.sh pushes it.

DAEMON=com.canonical.Myna.Dictation
DAEMON_PATH=/com/canonical/Myna/Dictation
FIELD=com.canonical.Myna.E2eField
FIELD_PATH=/com/canonical/Myna/E2eField
FIELD_STATE=$HOME/myna-shot/out/field.json
MIC_SINK=myna-e2e-speaker

# Take the graphical session's own environment (display server, input method
# modules), as a desktop launch hands it to an application. `lxc exec` shells
# carry none of it.
session_env() {
    local line
    while IFS= read -r line; do
        case $line in
            WAYLAND_DISPLAY=*|DISPLAY=*|XAUTHORITY=*|XDG_CURRENT_DESKTOP=*|XDG_SESSION_TYPE=*|\
GTK_IM_MODULE=*|QT_IM_MODULE=*|XMODIFIERS=*|XDG_DATA_DIRS=*)
                export "${line?}" ;;
        esac
    done < <(systemctl --user show-environment)
}

# wait_until SECONDS COMMAND...: poll until the command succeeds. Every wait
# is bounded; on timeout the caller's assertion fails.
wait_until() {
    local deadline=$((SECONDS + $1)); shift
    until "$@"; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}

dictation_prop() {
    gdbus call --session --dest $DAEMON --object-path $DAEMON_PATH \
        --method org.freedesktop.DBus.Properties.Get $DAEMON "$1" | sed -E "s/^\(<'?([^'>]*)'?>,\)$/\1/"
}
dictation_state_is() { [ "$(dictation_prop State)" = "$1" ]; }
# The captured level: non-zero only if the virtual microphone's audio arrives.
dictation_hears_audio() { awk -v p="$(dictation_prop AudioPeak)" 'BEGIN { exit !(p > 0.01) }'; }
dictation_toggle() {
    gdbus call --session --dest $DAEMON --object-path $DAEMON_PATH \
        --method $DAEMON.Toggle >/dev/null
}

# The speech clip, looped on the virtual speaker whose monitor is the default
# microphone: the speaker never falls silent, so no case races the clip.
mic_start() {
    ( while :; do pw-cat --playback --target $MIC_SINK "$1" || sleep 1; done ) >/dev/null 2>&1 &
    echo $! > "$HOME/myna-shot/mic.pid"
}

mic_stop() {
    kill "$(cat "$HOME/myna-shot/mic.pid")" 2>/dev/null
    pkill -x pw-cat
    true
}

field_start() {
    mkdir -p "$HOME/myna-shot/out"
    rm -f "$FIELD_STATE"
    session_env
    # GTK would otherwise parse the desktop's (GTK3) theme and warn about it.
    GTK_THEME=Adwaita setsid "$HOME/myna-shot/field-app.py" "$FIELD_STATE" \
        >"$HOME/myna-shot/out/field-app.log" 2>&1 </dev/null &
    wait_until 30 test -s "$FIELD_STATE"
}
field_action() { # NAME [FIELD]
    local param='[]'
    [ -z "${2:-}" ] || param="[<\"$2\">]"
    gdbus call --session --dest $FIELD --object-path $FIELD_PATH \
        --method org.freedesktop.Application.ActivateAction "$1" "$param" '{}' >/dev/null
}
field_get() { jq -r ".$1" "$FIELD_STATE"; }
field_is() { [ "$(field_get "$1")" = "$2" ]; }
# Focus a field and wait until the toolkit confirms the window is the active
# one and the field holds the focus.
field_focus() {
    field_action focus "$1"
    wait_until 20 field_is focus "$1" && wait_until 20 field_is active true
}
field_clear() { field_action clear; wait_until 5 field_is plain ""; }
