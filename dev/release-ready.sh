#!/bin/sh
# Succeed once everything a release publishes serves <version>, which is when
# its draft GitHub Release may go public (.kb/releasing.md).
#
#   dev/release-ready.sh <X.Y.Z>
#
# The snaps are the ones snaps.yml builds, on the store's edge channel for
# amd64 and arm64. The deb is ready when the PPA publishes no myna-config
# binary of another version, so a series or architecture still building, or
# one left behind on an old version, holds the release back. Exits 1 when
# something lags, naming it, and 2 when the store or Launchpad cannot be read.
set -eu
version=$1
root=$(cd "$(dirname "$0")/.." && pwd)
ppa='~canonical-desktop-team/+archive/ubuntu/myna'
lagging=0

lag() {
    echo "not yet: $1"
    lagging=1
}

snaps=$(sed -n 's/^ *snap: \[\(.*\)\]$/\1/p' "$root/.github/workflows/snaps.yml" | head -n 1 | tr -d ',')
[ -n "$snaps" ] || { echo "error: no snap list in snaps.yml" >&2; exit 2; }
for snap in $snaps; do
    name=$(awk '/^name:/ {print $2; exit}' "$root/$snap-snap/snap/snapcraft.yaml")
    info=$(curl -fsS --max-time 60 -H 'Snap-Device-Series: 16' \
        "https://api.snapcraft.io/v2/snaps/info/$name?fields=version") || exit 2
    for arch in amd64 arm64; do
        served=$(echo "$info" | jq -r --arg arch "$arch" '[."channel-map"[]
            | select(.channel.track == "latest" and .channel.risk == "edge" and .channel.architecture == $arch)
            | .version] | first // "nothing"')
        [ "$served" = "$version" ] || lag "$name on $arch serves $served"
    done
done

# The nonce defeats the cache anonymous Launchpad reads are served from.
binaries=$(curl -fsS --max-time 60 "https://api.launchpad.net/devel/$ppa?ws.op=getPublishedBinaries&binary_name=myna-config&exact_match=true&status=Published&nonce=$$") || exit 2
count=$(echo "$binaries" | jq '.entries | length')
[ "$count" -gt 0 ] || lag "the PPA publishes no myna-config"
stale=$(echo "$binaries" | jq -r --arg v "$version-" '.entries[]
    | select(.binary_package_version | startswith($v) | not)
    | "\(.distro_arch_series_link | split("/")[-2:] | join("/")) serves \(.binary_package_version)"')
[ -z "$stale" ] || echo "$stale" | while read -r line; do echo "not yet: PPA $line"; done
[ -z "$stale" ] || lagging=1

[ "$lagging" = 0 ] || exit 1
echo "ready: the store and the PPA serve $version"
