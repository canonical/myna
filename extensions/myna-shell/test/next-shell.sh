#!/bin/bash
# next-shell.sh - run the extension suite against the NEXT GNOME Shell, in a
# throwaway container.
#
#     test/next-shell.sh                 (from extensions/myna-shell/)
#
# exits 0 when the suite passed there, non-zero otherwise.
#
# metadata.json declares Shell 50 and 51, and only 50 is reachable from a
# Workshop: `workshop init` takes ubuntu@20.04, 22.04, 24.04 or 26.04 and
# nothing newer, so the myna-shell workshop's 26.04 base pins it to Shell 50.
# The host's mutter APIs (Meta.WaylandClient.new_subprocess, owns_window,
# hide_from_window_list, ...) exist in both the mutter 18 (Shell 50) and 51
# (Shell 51) ABIs, so a suite that passes on 50 must also load on 51.
#
# Ubuntu 26.10 (stonking) carries Shell 51, so this borrows a container of it
# and runs test/run-suite.sh inside. Nothing but gjs, gnome-shell and
# dbus-run-session is installed: the host's pure GJS suites need no display
# server of their own.
#
# It is a development series, so it will break for reasons that are not ours.
# Treat a failure here as a question, not a verdict - CI runs it non-blocking
# for the same reason.
#
# LXD and not Docker, in CI as well as here. GNOME Shell reaches logind over
# the system bus at startup, and a Docker container has neither: the Shell
# gets far enough to create a surfaceless renderer and then dies in
# `LoginManagerSystemd`. A pure-gjs suite would not catch that (it never
# starts a Shell), so there is one runtime, and it is the one that can host
# a session.
#
#   MYNA_SHELL_NEXT_KEEP=1   leave the container behind for a second look
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC=$(dirname "$HERE")
ROOT=$(cd "$SRC/../.." && pwd)
NAME=myna-shell-next
PACKAGES="gjs gnome-shell dbus-daemon"
IMAGE=ubuntu-daily:stonking
# The suite runs from the mount, so the container needs no write access to it.
INNER="/project/extensions/myna-shell/test/run-suite.sh"

if ! command -v lxc >/dev/null 2>&1 || ! lxc list >/dev/null 2>&1; then
    echo "next-shell: no usable LXD; skipping" >&2
    exit 77
fi

run_suite() {
    if ! lxc info "$NAME" >/dev/null 2>&1; then
        lxc launch "$IMAGE" "$NAME" || return 1
        # cloud-init owns apt until it is done; racing it wedges dpkg.
        lxc exec "$NAME" -- cloud-init status --wait >/dev/null 2>&1
        lxc exec "$NAME" -- bash -c "
            set -e
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y -qq --no-install-recommends $PACKAGES" || return 1
    fi
    lxc config device remove "$NAME" project >/dev/null 2>&1
    lxc config device add "$NAME" project disk \
        source="$ROOT" path=/project readonly=true >/dev/null || return 1
    lxc exec "$NAME" -- gnome-shell --version
    lxc exec "$NAME" -- "$INNER"
}

rc=0
run_suite || rc=$?

[ "${MYNA_SHELL_NEXT_KEEP:-}" = 1 ] || lxc delete --force "$NAME" >/dev/null 2>&1

exit "$rc"
