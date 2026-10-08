#!/usr/bin/env bash
#
# Replays exactly what myna-desktop's `pick_address` does (client/myna-desktop/
# src/inject/ibus.rs) against the machine it runs on, from the host AND from
# inside the snap's confinement, and says which of the two liveness checks
# (daemon PID, socket path) fails and for which address file. `--prune` then
# removes the address files whose daemon is provably gone.
#
#   bash ibus-doctor.sh          # report only
#   bash ibus-doctor.sh --prune  # report, then delete dead address files
set -uo pipefail

PRUNE=0
[ "${1:-}" = "--prune" ] && PRUNE=1

say() { printf '\n== %s ==\n' "$1"; }
# A D-Bus address percent-encodes every byte outside [-0-9A-Za-z_/.\]: an `@` in
# the home path (any AD login) arrives as %40. Decode before looking on disk.
undbus() { printf '%b' "$(printf '%s' "$1" | sed 's/%\([0-9a-fA-F]\{2\}\)/\\x\1/g')"; }
ok() { printf '  ok    %s\n' "$1"; }
bad() { printf '  FAIL  %s\n' "$1"; }
info() { printf '        %s\n' "$1"; }

say "1. session"
info "user            $(id -un) ($(id -u))"
info "session type    ${XDG_SESSION_TYPE:-<unset>}"
info "desktop         ${XDG_CURRENT_DESKTOP:-<unset>}"
info "WAYLAND_DISPLAY ${WAYLAND_DISPLAY:-<unset>}"
info "DISPLAY         ${DISPLAY:-<unset>}"
info "snapd           $(snap version 2>/dev/null | awk '/^snapd/{print $2}')"
info "ibus            $(ibus version 2>/dev/null | head -1)"

say "2. live ibus daemons"
if pgrep -a ibus-daemon; then :; else
  bad "no ibus-daemon is running for anyone on this machine"
  info "nothing myna can connect to; every address file will look stale"
fi

say "3. myna dictation daemon"
MPID=$(systemctl --user show -p MainPID --value snap.myna.myna.service 2>/dev/null)
if [ -n "${MPID:-}" ] && [ "$MPID" != "0" ]; then
  info "pid             $MPID (started $(ps -o lstart= -p "$MPID" 2>/dev/null | xargs))"
  denv() { tr '\0' '\n' < "/proc/$MPID/environ" 2>/dev/null | sed -n "s/^$1=//p"; }
  D_HOME=$(denv HOME); D_XDG=$(denv XDG_CONFIG_HOME)
  D_WL=$(denv WAYLAND_DISPLAY); D_X11=$(denv DISPLAY); D_ADDR=$(denv IBUS_ADDRESS)
  D_REAL=$(denv SNAP_REAL_HOME)
  info "HOME            ${D_HOME:-<unset>}"
  info "SNAP_REAL_HOME  ${D_REAL:-<unset>}"
  info "XDG_CONFIG_HOME ${D_XDG:-<unset>}"
  info "WAYLAND_DISPLAY ${D_WL:-<unset>}"
  info "DISPLAY         ${D_X11:-<unset>}"
  info "IBUS_ADDRESS    ${D_ADDR:-<unset>}"
  if [ -z "$D_WL" ] && [ -z "$D_X11" ]; then
    bad "the daemon sees no display: address files cannot be ranked by session"
    info "it will take the newest live daemon on the machine, whichever session that is"
  fi
else
  bad "snap.myna.myna.service is not running (MainPID=${MPID:-none})"
  info "using this shell's environment for the checks below instead"
  D_HOME=$HOME; D_REAL=$HOME; D_ADDR=${IBUS_ADDRESS:-}; D_XDG=${XDG_CONFIG_HOME:-}; D_WL=${WAYLAND_DISPLAY:-}; D_X11=${DISPLAY:-}
fi

say "4. address files myna would search"
# candidate_dirs(): $XDG_CONFIG_HOME/ibus/bus, $HOME/.config/ibus/bus, and
# $SNAP_REAL_HOME/.config/ibus/bus.
DIRS=()
[ -n "${D_XDG:-}" ] && DIRS+=("$D_XDG/ibus/bus")
[ -n "${D_HOME:-}" ] && DIRS+=("$D_HOME/.config/ibus/bus")
[ -n "${D_REAL:-}" ] && DIRS+=("$D_REAL/.config/ibus/bus")
FILES=()
seen=""
for d in "${DIRS[@]}"; do
  [ -d "$d" ] || { info "absent  $d"; continue; }
  info "search  $d"
  for f in "$d"/*; do
    [ -f "$f" ] || continue
    # $HOME/.config/ibus/bus is often a symlink to the real one (the gnome
    # extension's launcher makes it): count each address file once.
    r=$(readlink -f "$f")
    case " $seen " in *" $r "*) continue ;; esac
    seen="$seen $r"
    FILES+=("$f")
  done
done
[ ${#FILES[@]} -eq 0 ] && { bad "no address files at all: myna reports \"no IBus address file in ...\", not this error"; exit 0; }

# want: the file-name suffix for this session's display.
WANT=""
[ -n "${D_WL:-}" ] && WANT="unix-$D_WL"
[ -z "$WANT" ] && [ -n "${D_X11:-}" ] && WANT="unix${D_X11//:/-}"
info "display suffix  ${WANT:-<none: rank by mtime only>}"

# Rank: display matches first, newest first within each group (ibus.rs:318-333).
rank() {
  local group=$1 f
  for f in "${FILES[@]}"; do
    case "$group:${WANT:+match}" in
      hit:match) [[ "$f" == *"$WANT" ]] || continue ;;
      hit:) continue ;;
      miss:match) [[ "$f" == *"$WANT" ]] && continue ;;
    esac
    printf '%s\t%s\n' "$(stat -c %Y "$f")" "$f"
  done | sort -rn | cut -f2
}
RANKED=$( { rank hit; rank miss; } )

say "5. per-file verdict (myna takes the first that passes both)"
PICKED=""; FIRST_STALE=""; ENV_LIVE=0
PROBE=()   # "<host 1|0>|<path>" for every path re-checked under confinement
# An IBUS_ADDRESS in the daemon's environment comes first while its socket
# exists. It is frozen at the daemon's start, so it outlives the IBus it named.
if [ -n "${D_ADDR:-}" ]; then
  esock=${D_ADDR#*path=}; esock=${esock%%,*}
  if [ "$esock" = "$D_ADDR" ]; then
    ENV_LIVE=1; ok "IBUS_ADDRESS is abstract: myna uses it unchecked and ignores the files"
  elif [ -e "$(undbus "$esock")" ]; then
    ENV_LIVE=1; PROBE+=("1|$(undbus "$esock")")
    ok "IBUS_ADDRESS socket exists: myna uses it and ignores the files"
  else
    bad "IBUS_ADDRESS socket $(undbus "$esock") is MISSING"
    info "older myna builds fail every press with \"cannot connect to IBus:"
    info "I/O error\"; current ones fall back to the files below. Fix:"
    info "\`systemctl --user unset-environment IBUS_ADDRESS\`, then restart the daemon."
  fi
fi
while IFS= read -r f; do
  [ -n "$f" ] || continue
  addr=$(sed -n 's/^IBUS_ADDRESS=//p' "$f")
  pid=$(sed -n 's/^IBUS_DAEMON_PID=//p' "$f" | tr -d ' ')
  sock=${addr#*path=}; sock=${sock%%,*}
  [ "$sock" = "$addr" ] && sock=""   # abstract socket: myna does not check it
  raw_sock=$sock
  [ -n "$sock" ] && sock=$(undbus "$sock")
  pid_ok=1; sock_ok=1
  [ -n "$pid" ] && { [ -e "/proc/$pid" ] || pid_ok=0; }
  [ -n "$sock" ] && { [ -e "$sock" ] || sock_ok=0; }
  PROBE+=("1|$f")
  [ -n "$pid" ] && PROBE+=("$pid_ok|/proc/$pid")
  [ -n "$sock" ] && PROBE+=("$sock_ok|$sock")
  printf '\n  %s\n' "$(basename "$f")"
  info "mtime  $(stat -c %y "$f" | cut -d. -f1)"
  info "pid    ${pid:-<none>} $([ $pid_ok = 1 ] && echo alive || echo 'GONE (no /proc entry)')"
  info "socket ${sock:-<abstract, unchecked>} $([ -n "$sock" ] && { [ $sock_ok = 1 ] && echo exists || echo MISSING; })"
  [ "$raw_sock" != "$sock" ] && info "       (percent-encoded in the file as $raw_sock)"
  if [ $pid_ok = 1 ] && [ $sock_ok = 1 ]; then
    if [ -z "$PICKED" ]; then PICKED=$f
      if [ $ENV_LIVE = 1 ]; then info "alive, the fallback should IBUS_ADDRESS die"
      else ok "myna connects to this one"; fi
    else info "alive, but not reached (an earlier file won)"; fi
  else
    [ -z "$PICKED" ] && [ -z "$FIRST_STALE" ] && FIRST_STALE=$f
    bad "rejected"
  fi
done <<< "$RANKED"

say "6. what myna reports"
if [ $ENV_LIVE = 1 ]; then
  ok "injection should work, via IBUS_ADDRESS"
elif [ -n "$PICKED" ]; then
  ok "injection should work, via $(basename "$PICKED")"
  if [ -n "$FIRST_STALE" ] && [ "$FIRST_STALE" != "$PICKED" ]; then
    info "it fell through $(basename "$FIRST_STALE") first. If the picked file"
    info "belongs to another session, injection lands in that session's focus."
  fi
else
  bad "\"IBus address file(s) present but the daemon looks gone (stale PID $(sed -n 's/^IBUS_DAEMON_PID=//p' "$FIRST_STALE" | tr -d ' ') / missing socket)\""
  info "every address file failed. Fix: start/restart ibus (\`ibus restart\`),"
  info "then re-run. If no ibus-daemon exists in this session at all (see 2),"
  info "the session simply has no IBus and the files are leftovers: --prune."
fi

say "7. the same paths, re-checked inside the snap's confinement"
if command -v snap >/dev/null 2>&1 && snap list myna >/dev/null 2>&1; then
  CONF=$(for e in "${PROBE[@]}"; do
           q=${e#*|}
           printf 'test -e %q && echo "1 %s" || echo "0 %s"\n' "$q" "$q" "$q"
         done | timeout 60 snap run --shell myna.myna 2>&1)
  diffs=0
  for e in "${PROBE[@]}"; do
    host=${e%%|*}; path=${e#*|}
    line=$(printf '%s\n' "$CONF" | grep -F -- " $path" | head -1)
    if [ -z "$line" ]; then
      bad "no answer for $path"; diffs=$((diffs+1)); continue
    fi
    if [ "${line%% *}" != "$host" ]; then
      bad "$path: host says $([ "$host" = 1 ] && echo present || echo absent), the snap says $([ "${line%% *}" = 1 ] && echo present || echo absent)"
      diffs=$((diffs+1))
    fi
  done
  if [ $diffs = 0 ]; then
    ok "confinement sees exactly what the host sees (${#PROBE[@]} paths)"
  else
    info "a path the host has but the snap cannot see is an AppArmor problem,"
    info "not a stale daemon: check section 9 and \`snap connections myna\`."
  fi
  printf '%s\n' "$CONF" | grep -v '^[01] ' | sed 's/^/  note  /'
else
  info "myna is not installed as a snap; skipped"
fi

say "8. connect to the picked address, host vs inside the snap"
# Sections 5 and 7 only stat the socket, which AppArmor barely mediates; a
# denied connect(2) passes them and still fails every press with "cannot
# connect to IBus: I/O error". Do the real connect plus the D-Bus EXTERNAL
# handshake, the first thing zbus does.
CONNECT_PY='
import os, socket, sys
from urllib.parse import unquote as undbus
addr = sys.argv[1]
kv = dict(p.split("=", 1) for p in addr.split(":", 1)[1].split(",") if "=" in p)
if "path" in kv: target = undbus(kv["path"])
elif "abstract" in kv: target = "\0" + undbus(kv["abstract"])
else: print("unsupported address " + addr); sys.exit(1)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.settimeout(5)
try: s.connect(target)
except OSError as e: print("connect failed: %s" % e); sys.exit(1)
s.sendall(b"\0AUTH EXTERNAL " + str(os.getuid()).encode().hex().encode() + b"\r\n")
try: reply = s.recv(256).decode(errors="replace").strip()
except OSError as e: print("handshake failed: %s" % e); sys.exit(1)
if reply.startswith("OK"): print("ok: " + reply); sys.exit(0)
print("handshake refused: " + (reply or "<connection closed>")); sys.exit(1)
'
if [ $ENV_LIVE = 1 ]; then ADDR=$D_ADDR
elif [ -n "$PICKED" ]; then ADDR=$(sed -n 's/^IBUS_ADDRESS=//p' "$PICKED")
else ADDR=""; fi
if [ -z "$ADDR" ]; then
  info "no address passed section 5; nothing to connect to"
else
  info "address  $ADDR"
  if out=$(python3 -c "$CONNECT_PY" "$ADDR" 2>&1); then ok "host  $out"; else bad "host  $out"; fi
  if command -v snap >/dev/null 2>&1 && snap list myna >/dev/null 2>&1; then
    # The snap shell reads its commands from stdin, so the program travels
    # base64-encoded; the $(...) is meant for that shell, not this one.
    # shellcheck disable=SC2016
    out=$(printf 'python3 -c "$(echo %s | base64 -d)" %q\n' \
            "$(printf '%s' "$CONNECT_PY" | base64 -w0)" "$ADDR" | timeout 30 snap run --shell myna.myna 2>&1)
    case "$out" in
      ok:*) ok "snap  $out" ;;
      *)    bad "snap  $out"
            info "host ok but snap failing is confinement: see section 9 for the denial" ;;
    esac
  else
    info "myna is not installed as a snap; skipped the confined connect"
  fi
fi

say "9. AppArmor denials for myna"
journalctl -k --since "-2 hours" --no-pager 2>/dev/null | grep -iE 'apparmor="DENIED".*profile="snap\\.myna\\.' | tail -20 \
  || info "none in the last 2 hours (or no journal access)"
snap connections myna 2>/dev/null | grep -E 'desktop-legacy' || info "desktop-legacy connection not found"

if [ $PRUNE = 1 ]; then
  say "10. prune"
  DEAD=()
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    pid=$(sed -n 's/^IBUS_DAEMON_PID=//p' "$f" | tr -d ' ')
    if [ -n "$pid" ] && [ ! -e "/proc/$pid" ]; then
      DEAD+=("$(readlink -f "$f")"); info "dead  $(basename "$f") (pid $pid)"
    else
      info "keep  $(basename "$f")"
    fi
  done <<< "$RANKED"
  if [ ${#DEAD[@]} -eq 0 ]; then
    info "nothing to remove"
  else
    printf '\n  remove %d address file(s)? ibus rewrites the live one on demand [y/N] ' "${#DEAD[@]}"
    read -r reply
    case "$reply" in
      [yY]*) for f in "${DEAD[@]}"; do rm -f -- "$f" && info "removed $f"; done
             info "now run \`ibus restart\` and try dictation again" ;;
      *)     info "left alone" ;;
    esac
  fi
fi
