#!/bin/bash
# snaps.sh fetch|install <dir> - the snaps a workshop launch needs, kept in
# <dir> with their assertions so they install without the store.
#
#   fetch    Downloads LXD 6/stable, Workshop, their bases and snapd into
#            <dir>.new and, if every download succeeds and the revisions
#            differ from <dir>'s, replaces <dir> and prints "changed".
#   install  Installs <dir>'s set offline, then does what launch-workshop
#            skips when it finds them installed.
set -euo pipefail
cmd=$1
dir=$2

# A degraded store answers some requests just past snapd's own timeout.
download() {
    local try
    for try in 1 2 3 4; do
        timeout 120 snap download --target-directory="$new" --basename="$1" \
            ${2:+--channel="$2"} "$1" </dev/null >/dev/null && return
        [ "$try" -lt 4 ] && sleep 10
    done
    return 1
}

case $cmd in
fetch)
    new=$dir.new
    trap 'rm -rf "$new"' EXIT
    rm -rf "$new"
    mkdir -p "$new"
    download lxd 6/stable
    download workshop latest/stable
    for s in lxd workshop; do
        unsquashfs -q -cat "$new/$s.snap" meta/snap.yaml | awk '$1 == "base:" {print $2}'
    done | sort -u > "$new/bases"
    while read -r base; do download "$base"; done < "$new/bases"
    download snapd
    for a in "$new"/*.assert; do
        echo "$(basename "$a" .assert) $(awk '$1 == "snap-revision:" {print $2}' "$a")"
    done > "$new/revisions"
    if ! cmp -s "$new/revisions" "$dir/revisions" 2>/dev/null; then
        rm -rf "$dir"
        mv "$new" "$dir"
        echo changed
    fi
    ;;
install)
    cd "$dir"
    for a in *.assert; do sudo snap ack "$a"; done
    sudo snap install snapd.snap
    while read -r base; do sudo snap install "$base.snap"; done < bases
    sudo snap install lxd.snap
    sudo snap install --classic workshop.snap
    # A file install tracks no channel, and launch-workshop refreshes LXD
    # from the store unless it tracks 6/stable.
    sudo snap switch --channel=6/stable lxd
    sudo snap switch --channel=latest/stable workshop
    sudo snap refresh --hold=24h
    sudo lxd waitready
    # launch-workshop opens the runner's firewall to the workshop bridge
    # only when it installs Workshop itself.
    for tool in iptables ip6tables; do
        sudo "$tool" -nL DOCKER-USER >/dev/null 2>&1 || continue
        sudo "$tool" -I DOCKER-USER -i workshopbr0 -j ACCEPT
        sudo "$tool" -I DOCKER-USER -o workshopbr0 -m conntrack \
            --ctstate RELATED,ESTABLISHED -j ACCEPT
    done
    ;;
*)
    echo "usage: $0 fetch|install <dir>" >&2
    exit 2
    ;;
esac
