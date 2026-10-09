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

# A virtual keyboard on the real seat (uinput-keys.py, as root): chords go
# through the compositor like a person's, so shortcuts and focus behave.
KEYS_FIFO=$HOME/myna-shot/keys.fifo
keys_start() {
    rm -f "$KEYS_FIFO" "$HOME/myna-shot/out/uinput.log"
    mkfifo "$KEYS_FIFO"
    # shellcheck disable=SC2024 # the log belongs to the user
    sudo setsid python3 "$HOME/myna-shot/uinput-keys.py" "$KEYS_FIFO" \
        >"$HOME/myna-shot/out/uinput.log" 2>&1 </dev/null &
    wait_until 20 grep -qx ready "$HOME/myna-shot/out/uinput.log"
}
# shellcheck disable=SC2016 # expands in the inner shell
key() { timeout 5 sh -c 'echo "$1" > "$2"' sh "$1" "$KEYS_FIFO"; }

# Bind CHORD (e.g. "<Super>j") to the daemon's Toggle in the desktop's own
# shortcut store, the way a user's custom shortcut is.
bind_toggle_key() {
    local cmd="gdbus call --session --dest $DAEMON --object-path $DAEMON_PATH --method $DAEMON.Toggle"
    case ${XDG_CURRENT_DESKTOP:-} in
        *GNOME*)
            local base=org.gnome.settings-daemon.plugins.media-keys
            local path=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/e2e/
            gsettings set "$base.custom-keybinding:$path" name 'Myna e2e'
            gsettings set "$base.custom-keybinding:$path" command "$cmd"
            gsettings set "$base.custom-keybinding:$path" binding "$1"
            gsettings set $base custom-keybindings "['$path']" ;;
        *XFCE*)
            xfconf-query -c xfce4-keyboard-shortcuts -n -t string -p "/commands/custom/$1" -s "$cmd" ;;
        *) echo "bind_toggle_key: unknown desktop ${XDG_CURRENT_DESKTOP:-}" >&2; return 1 ;;
    esac
}

# The daemon's journal from a mark on: `journal_mark`, then `journal_count RE`.
JOURNAL_MARK=$HOME/myna-shot/journal.mark
journal_mark() { date +%s.%N > "$JOURNAL_MARK"; }
journal_count() {
    journalctl --user -u snap.myna.myna.service --no-pager --since "@$(cat "$JOURNAL_MARK")" \
        | grep -c -- "$1"
}

# Restart the session's ibus-daemon and wait for it to serve again, so the
# next activation is its first: it has not read the engine's properties yet.
# The daemon re-executes itself, keeping its PID; its address (a new guid)
# is what changes.
ibus_restart() {
    local before
    # The bus address file is named after the display (ibus 1.5.34 names it
    # for WAYLAND_DISPLAY), so the CLI needs the session's environment.
    session_env
    before=$(ibus address)
    ibus restart >/dev/null 2>&1
    wait_until 20 sh -c "[ \"\$(ibus address)\" != '$before' ] && ibus engine >/dev/null"
}

# The virtual microphone's node id. On PipeWire 1.6 wpctl files a loopback's
# source under Filters, which the session manager never picks as the default.
mic_node_id() { pw-dump | jq -er '.[] | select(.info.props["node.name"] == "myna-e2e-mic") | .id'; }
# Make it the default source ourselves (waiting for the node to exist).
mic_make_default() {
    wait_until 20 mic_node_id && wpctl set-default "$(mic_node_id)"
}

default_source_is() { wpctl inspect @DEFAULT_AUDIO_SOURCE@ | grep -q "node.name = \"$1\""; }

# The HUD, hosted the way the myna-config deb's autostart entry hosts it on
# Xfce: the supervisor starts myna.hud while the daemon owns its bus name.
# (GNOME's shell extension does this there; the VM has no extension.)
hud_host_start() {
    mkdir -p "$HOME/myna-shot/out"
    session_env
    setsid "$HOME/myna-shot/myna-hud-host" >"$HOME/myna-shot/out/hud-host.log" 2>&1 </dev/null &
    wait_until 30 pgrep -u "$USER" -x myna-hud
}
# The pill is a mapped window only while the HUD has something to say.
pill_shows() { DISPLAY=${DISPLAY:-:0} xdotool search --onlyvisible --name '^myna-hud$' >/dev/null; }

# A checkpoint screenshot of the whole X11 desktop, collected by run-suite.sh
# as artifacts/screenshots/dictation/NAME.png.
checkpoint() {
    mkdir -p "$HOME/myna-shot/out/shots/dictation"
    import -window root "$HOME/myna-shot/out/shots/dictation/$1.png"
}

# Hold the backend still, so the daemon waits in `finalizing` for as long as a
# screenshot takes (the fake's transcript is otherwise back in milliseconds).
# The backend runs as root; the bracket keeps pkill from matching its own
# command line.
backend_freeze() { sudo pkill -STOP -f '[-]-adapter fake'; }
backend_thaw() { sudo pkill -CONT -f '[-]-adapter fake'; }

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
