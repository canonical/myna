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
        --method org.freedesktop.DBus.Properties.Get $DAEMON "$1" 2>/dev/null | sed -E "s/^\(<'?([^'>]*)'?>,\)$/\1/"
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
        *KDE*)
            # A command shortcut, as Plasma's Shortcuts settings make one:
            # kglobalaccel finds the entry through KSycoca (up to 1.5 s),
            # and only the `_launch` its entry gives it is grabbed. Keys are
            # Qt's, four to a sequence (fewer abort KWin).
            local key entry=myna-e2e-toggle.desktop dir=$HOME/.local/share/applications
            case $1 in '<Super>j') key=$((0x10000000 + 0x4a)) ;; *) echo "bind_toggle_key: no Qt key for $1" >&2; return 1 ;; esac
            mkdir -p "$dir"
            printf '[Desktop Entry]\nType=Application\nName=Myna e2e\nExec=%s\nNoDisplay=true\nX-KDE-GlobalAccel-CommandShortcut=true\n' \
                "$cmd" > "$dir/$entry"
            wait_until 10 kglobalaccel_registers "$entry"
            kglobalaccel unregister "$entry" '' >/dev/null
            kglobalaccel setForeignShortcutKeys "['$entry', '_launch', '', '']" "[([$key, 0, 0, 0],)]" >/dev/null ;;
        *) echo "bind_toggle_key: unknown desktop ${XDG_CURRENT_DESKTOP:-}" >&2; return 1 ;;
    esac
}

kglobalaccel() { # METHOD ARGS...
    local method=$1; shift
    gdbus call --session --dest org.kde.kglobalaccel --object-path /kglobalaccel \
        --method "org.kde.KGlobalAccel.$method" "$@"
}
kglobalaccel_registers() {
    kglobalaccel doRegister "['$1', '', 'Myna e2e', '']" >/dev/null && kglobalaccel getComponent "$1" >/dev/null
}

# The daemon's journal from a mark on: `journal_mark`, then `journal_count RE`.
JOURNAL_MARK=$HOME/myna-shot/journal.mark
journal_mark() { date +%s.%N > "$JOURNAL_MARK"; }
journal_count() {
    journalctl --user -u snap.myna.myna.service --no-pager --since "@$(cat "$JOURNAL_MARK")" \
        | grep -c -- "$1"
}
# Faults the daemon logged this boot for audio the capture never received.
audio_lost_count() { journalctl --user -b -u snap.myna.myna.service --no-pager | grep -c 'ctrl: capture lost'; }

# A thread of PipeWire's graph runs realtime.
pipewire_realtime() { ps -L -o cls= -p "$(pgrep -d, -xu "$USER" pipewire)" | grep -qE 'FF|RR'; }

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

# Plasma (KWin) rather than an X11 desktop: the window checks below differ.
plasma() { [[ ${XDG_CURRENT_DESKTOP:-} == *KDE* ]]; }

# A KWin script: the compositor's own view of its windows, which Wayland
# gives no client. Its print()s land in the user journal, its name last.
kwin_script() {
    local name=myna-e2e-$$-$RANDOM js since kwin=(--session --dest org.kde.KWin --object-path /Scripting)
    js=$(mktemp --suffix=.js)
    printf '%s\nprint("%s");\n' "$1" "$name" > "$js"
    since=$(date +%s.%N)
    # shellcheck disable=SC2016 # expands in the inner shell
    gdbus call "${kwin[@]}" --method org.kde.kwin.Scripting.loadScript "$js" "$name" >/dev/null \
        && gdbus call "${kwin[@]}" --method org.kde.kwin.Scripting.start >/dev/null \
        && wait_until 5 sh -c 'journalctl --user --since "@$1" -o cat | grep -qx "js: $2"' sh "$since" "$name"
    gdbus call "${kwin[@]}" --method org.kde.kwin.Scripting.unloadScript "$name" >/dev/null
    rm -f "$js"
    journalctl --user --since "@$since" -o cat | sed -n "/^js: $name\$/d; s/^js: //p"
}
# The work area (`workarea X Y W H`), then every window KWin shows:
# ID CLASS ACTIVE WANTS_INPUT LAYER X Y W H (layer 9 is the overlay).
kwin_windows() {
    kwin_script 'const a = workspace.clientArea(KWin.MaximizeArea, workspace.activeScreen, workspace.currentDesktop);
print(["workarea", a.x, a.y, a.width, a.height].join(" "));
for (const w of workspace.windowList()) {
    const g = w.frameGeometry;
    print([w.internalId, w.resourceClass || "-", w === workspace.activeWindow ? 1 : 0,
           w.wantsInput ? 1 : 0, w.layer, g.x, g.y, g.width, g.height].join(" "));
}'
}
# KWin's focus-stealing prevention may leave a window launched from a shell
# inactive: make the one holding FIELD active, as a click would (the Qt field
# app gives `other` a window of its own).
field_window_activate() {
    local caption="Myna e2e fields"
    [ "$1" != other ] || caption="$caption (other)"
    plasma && kwin_script "for (const w of workspace.windowList())
    if (w.caption === '$caption') workspace.activeWindow = w;" >/dev/null
}

# The HUD, hosted the way the myna-config deb's autostart entry hosts it on
# Xfce and Plasma: the supervisor starts myna.hud while the daemon owns its
# bus name. (GNOME's shell extension does this there; the VM has no extension.)
hud_host_start() {
    mkdir -p "$HOME/myna-shot/out"
    session_env
    setsid "$HOME/myna-shot/myna-hud-host" >"$HOME/myna-shot/out/hud-host.log" 2>&1 </dev/null &
    wait_until 30 pgrep -u "$USER" -x myna-hud
}
# The HUD runs as KIND's host (`myna-hud --host KIND`).
hud_host_kind_is() { pgrep -u "$USER" -ax myna-hud | grep -q -- "--host $1\$"; }
# Log out of the desktop session; the host must see it end and stop.
session_logout() {
    if plasma; then
        gdbus call --session --dest org.kde.Shutdown --object-path /Shutdown --method org.kde.Shutdown.logout >/dev/null
    else
        xfce4-session-logout --logout --fast
    fi
}
hud_host_ended() { ! pgrep -u "$USER" -x myna-hud-host && ! pgrep -u "$USER" -x myna-hud; }
# The pill is a mapped window only while the HUD has something to say.
pill_shows() {
    if plasma; then
        kwin_windows | awk '$2 == "myna-hud" { found = 1 } END { exit !found }'
    else
        DISPLAY=${DISPLAY:-:0} xdotool search --onlyvisible --name '^myna-hud$' >/dev/null
    fi
}

# The HUD is a client of the daemon once it has said so (RegisterClient):
# until then the daemon answers a dictation with a notification toast instead
# of the pill. `journal_mark` before starting the host, then wait for this.
hud_registered() { [ "$(journal_count 'dbus: RegisterClient')" -ge 1 ]; }

# Every Notify call from now on lands in out/notify.log (dbus-monitor), after
# a positive control proves the monitor sees one.
NOTIFY_LOG=$HOME/myna-shot/out/notify.log
notify_control() {
    gdbus call --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify e2e-control 0 '' control '' '[]' '{}' 1000 >/dev/null 2>&1
    sleep 0.3
    grep -q e2e-control "$NOTIFY_LOG"
}
notify_watch_start() {
    mkdir -p "$HOME/myna-shot/out"
    : > "$NOTIFY_LOG"
    session_env
    setsid dbus-monitor --session "interface='org.freedesktop.Notifications',member='Notify'" \
        >"$NOTIFY_LOG" 2>&1 </dev/null &
    wait_until 30 notify_control
}
# Myna toasted nothing (the daemon falls back to notifications while no HUD
# is registered).
myna_toasts() { grep -c 'string "Myna"'; }
no_myna_toast() { [ "$(myna_toasts < "$NOTIFY_LOG")" = 0 ]; }
# The same, for what was sent after `notify_mark` (the watch keeps appending).
notify_mark() { wc -c < "$NOTIFY_LOG" > "$HOME/myna-shot/notify.mark"; }
no_myna_toast_since_mark() {
    [ "$(tail -c +$(($(cat "$HOME/myna-shot/notify.mark") + 1)) "$NOTIFY_LOG" | myna_toasts)" = 0 ]
}

# Restart the daemon the way the service manager does, and return once the
# new one owns its bus name.
daemon_restart() {
    systemctl --user restart snap.myna.myna.service
    wait_until 30 dictation_state_is idle
}
# The daemon with MYNA_DEBUG on or off (a service drop-in), restarted: the
# preedit it sends is logged only at that tier.
daemon_debug() {
    local dropin=$HOME/.config/systemd/user/snap.myna.myna.service.d
    case $1 in
        on) mkdir -p "$dropin" && printf '[Service]\nEnvironment=MYNA_DEBUG=1\n' > "$dropin/debug.conf" ;;
        off) rm -f "$dropin/debug.conf" ;;
    esac
    systemctl --user daemon-reload
    daemon_restart
}

# The fake backend serving its streaming script (one unstable hypothesis
# ahead of the first segment) or its batch one; the daemon restarted after,
# so it learns the backend's mode afresh.
fake_streaming() {
    sudo snap set myna-fake-backend streaming="$1"
    wait_until 30 test -S /var/snap/myna-fake-backend/common/share/provider/myna.sock
    daemon_restart
}

# GNOME Shell's own process: its PID, and whether it has logged the
# IBusAttrList double free (#18) since the journal mark. Its leaked wrapper is
# finalized at a GJS collection, which runs about every 10 s.
shell_pid() { pgrep -xu "$USER" gnome-shell; }
shell_attr_list_criticals() {
    journalctl --user --no-pager --since "@$(cat "$JOURNAL_MARK")" _COMM=gnome-shell \
        | grep -c -e 'IBusAttrList has been finalized' -e "assertion 'G_IS_OBJECT (object)' failed"
}

# The pill is up while the daemon is still listening: it was not too late.
pill_while_listening() { pill_shows && dictation_state_is recording; }

pill_id() { DISPLAY=${DISPLAY:-:0} xdotool search --onlyvisible --name '^myna-hud$' | head -1; }
# The active window's id, and whether it is the pill.
active_window() {
    if plasma; then
        kwin_windows | awk '$3 == 1 { print $1, ($2 == "myna-hud") }'
    else
        local id
        id=$(DISPLAY=${DISPLAY:-:0} xprop -root _NET_ACTIVE_WINDOW | sed 's/.*# //')
        echo "$id $([ "$id" = "$(printf '0x%x' "$(pill_id)")" ] && echo 1 || echo 0)"
    fi
}
active_window_save() { active_window > "$HOME/myna-shot/active.window"; }
# The active window is still the one `active_window_save` saw, and not the pill.
active_window_kept() {
    local now
    now=$(active_window)
    [ "$now" = "$(cat "$HOME/myna-shot/active.window")" ] && [ "${now##* }" = 0 ]
}

# The pill as the window manager sees it: mapped, a window that takes no
# input focus (an X11 notification-type window; a layer surface on KWin's
# overlay layer), bottom centre inside the work area (the panel is out of
# it). The numbers go to out/pill.txt for a human.
pill_placed() {
    local id out=$HOME/myna-shot/out/pill.txt x y w h wx wy ww wh
    if plasma; then
        kwin_windows > "$out"
        read -r x y w h < <(awk '$2 == "myna-hud" && $4 == 0 && $5 == 9 { print $6, $7, $8, $9 }' "$out")
        read -r wx wy ww wh < <(awk '$1 == "workarea" { print $2, $3, $4, $5 }' "$out")
        [ -n "$x" ] && [ -n "$wx" ] || return 1
    else
        export DISPLAY=${DISPLAY:-:0}
        id=$(pill_id) && [ -n "$id" ] || return 1
        {
            xprop -id "$id" _NET_WM_WINDOW_TYPE WM_HINTS _NET_WM_STATE
            xwininfo -id "$id"
            xprop -root _NET_WORKAREA
        } > "$out" 2>&1
        grep -q '^_NET_WM_WINDOW_TYPE.*_NET_WM_WINDOW_TYPE_NOTIFICATION' "$out" || return 1
        grep -q 'Client accepts input or input focus: False' "$out" || return 1
        grep -q 'Map State: IsViewable' "$out" || return 1
        x=$(sed -n 's/.*Absolute upper-left X: *//p' "$out")
        y=$(sed -n 's/.*Absolute upper-left Y: *//p' "$out")
        w=$(sed -n 's/^ *Width: *//p' "$out")
        h=$(sed -n 's/^ *Height: *//p' "$out")
        read -r wx wy ww wh < <(sed -n 's/^_NET_WORKAREA.*= *//p' "$out" | tr -d ',' | cut -d' ' -f1-4)
    fi
    echo "pill $w x $h at $x,$y; work area $ww x $wh at $wx,$wy" >> "$out"
    # Inside, centred within 32 px, and within 120 px of the bottom.
    [ "$x" -ge "$wx" ] && [ $((x + w)) -le $((wx + ww)) ] \
        && [ "$y" -ge "$wy" ] && [ $((y + h)) -le $((wy + wh)) ] \
        && [ $(( (2 * x + w) - (2 * wx + ww) )) -le 64 ] && [ $(( (2 * wx + ww) - (2 * x + w) )) -le 64 ] \
        && [ $(( wy + wh - (y + h) )) -le 120 ]
}

# The HUD host's log carries no GTK critical (a layout bug the HUD survives).
hud_log_clean() { ! grep -q 'CRITICAL' "$HOME/myna-shot/out/hud-host.log"; }

# A checkpoint screenshot of the whole desktop, collected by run-suite.sh as
# artifacts/screenshots/dictation/NAME.png. KWin lets only Spectacle take one.
checkpoint() {
    local png=$HOME/myna-shot/out/shots/dictation/$1.png
    mkdir -p "${png%/*}"
    if plasma; then
        spectacle -b -n -f -o "$png" >>"$HOME/myna-shot/out/spectacle.log" 2>&1
    else
        import -window root "$png"
    fi
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

# The field app in the desktop's own toolkit: Qt on Plasma, else GTK4.
field_start() {
    local toolkit=()
    mkdir -p "$HOME/myna-shot/out"
    rm -f "$FIELD_STATE"
    session_env
    ! plasma || toolkit=(--qt)
    # GTK would otherwise parse the desktop's (GTK3) theme and warn about it.
    GTK_THEME=Adwaita setsid "$HOME/myna-shot/field-app.py" "${toolkit[@]}" "$FIELD_STATE" \
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
    wait_until 20 field_is focus "$1" || return 1
    wait_until 5 field_is active true \
        || { field_window_activate "$1" && wait_until 15 field_is active true; }
}
field_clear() { field_action clear; wait_until 5 field_is plain ""; }
