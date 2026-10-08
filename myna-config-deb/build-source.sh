#!/bin/sh
# Stage the myna-config Debian source package from the committed tree.
#
# The deb builds a three-crate workspace (myna-core, myna-platform, myna-config) cut out of
# client/, with its crates.io dependencies vendored into the orig tarball so
# the build runs offline. Output, under target/deb/ by default:
#
#   myna-config_<upstream>.orig.tar.xz    reproducible: git archive + cargo vendor,
#                                         mtimes pinned to the commit date
#   myna-config-<upstream>/               the unpacked source with debian/ applied,
#                                         ready for `sbuild` run from inside it
#
# <upstream> is dev/version.sh's: X.Y.Z on the tag vX.Y.Z, which must match
# the changelog, and X.Y.Z+git<n>.<sha> past it. Every commit gets its own
# orig tarball, so Launchpad never sees two different tarballs under one name.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out=${1:-$root/target/deb}

if [ -n "$(git -C "$root" status --porcelain -- client myna-config-deb extensions/myna-shell dev/stage-extension.sh dev/check-shell-version.sh)" ]; then
    echo "warning: uncommitted changes under client/, myna-config-deb/, extensions/myna-shell/ or the dev/ staging scripts are not staged (git archive reads HEAD)" >&2
fi

changelog_version=$(dpkg-parsechangelog -l "$here/debian/changelog" -S Version)
revision=${changelog_version##*-}
upstream=$("$root/dev/version.sh")
case $upstream in
*+git*) ;;
"${changelog_version%-*}") ;;
*)
    echo "error: HEAD is tagged v$upstream but debian/changelog releases $changelog_version" >&2
    exit 1
    ;;
esac
commit_time=$(git -C "$root" log -1 --format=%ct)
# PPA=N appends ~ppaN, which dput requires for a PPA target and which sorts
# below the archive version. SERIES=<name> retargets a series other than the
# changelog's, tagged ~<release> before ~ppaN so an older series sorts below
# a newer one: ~24.04~ppa1 < ~26.04~ppa1 < ~ppa1.
series=$(dpkg-parsechangelog -l "$here/debian/changelog" -S Distribution)
if [ -n "${SERIES:-}" ] && [ "$SERIES" != "$series" ]; then
    release=$(ubuntu-distro-info --series="$SERIES" -r | sed 's/ LTS$//')
    revision="$revision~$release"
    series=$SERIES
fi
if [ -n "${PPA:-}" ]; then
    # <n> in +git<n> rises only along one history: upload nothing main lacks.
    if ! git -C "$root" merge-base --is-ancestor HEAD origin/main; then
        echo "error: PPA uploads come from origin/main, and HEAD is not on it" >&2
        exit 1
    fi
    revision="$revision~ppa$PPA"
fi
version="$upstream-$revision"
stage="$out/myna-config-$upstream"

rm -rf "$out"
mkdir -p "$stage"
git -C "$root" archive HEAD client/Cargo.toml client/Cargo.lock client/build-support client/data client/myna-core client/myna-platform client/myna-config \
    | tar -x -C "$stage" --strip-components=1
# The binaries report the version build-support/version.rs finds staged here.
echo "$upstream" > "$stage/.version"

# The GNOME Shell extension, as gnome-shell loads it: a <uuid>/ directory
# with metadata.json stamped like .version, so debian/rules needs no python3.
ext_tmp=$(mktemp -d)
trap 'rm -rf "$ext_tmp"' EXIT
git -C "$root" archive HEAD extensions/myna-shell | tar -x -C "$ext_tmp"
"$root/dev/check-shell-version.sh" "$ext_tmp/extensions/myna-shell/metadata.json" "$series"
"$root/dev/stage-extension.sh" "$ext_tmp/extensions/myna-shell" "$stage/extensions" "$upstream" >/dev/null

# Tests that read the repository (snapcraft.yaml, docs, dev/) have nothing to read here.
rm "$stage/myna-config/tests/snap_packaging.rs" "$stage/myna-config/tests/client_version.rs"

members='members = ["myna-core", "myna-platform", "myna-config"]'
if [ "$(grep -c '^members = ' "$stage/Cargo.toml")" != 1 ]; then
    echo "error: expected exactly one workspace members line in client/Cargo.toml" >&2
    exit 1
fi
sed -i "s/^members = .*/$members/" "$stage/Cargo.toml"

# Prunes Cargo.lock to the members' closure; versions stay as locked.
# Filtered to the Ubuntu build targets: the winapi/windows-sys crates alone
# are 130 MB unpacked. gettext-sys carries a gettext tarball it builds only
# without the gettext-system feature, which this workspace always sets.
(cd "$stage" && cargo vendor-filterer \
    --platform=x86_64-unknown-linux-gnu \
    --platform=aarch64-unknown-linux-gnu \
    --platform=armv7-unknown-linux-gnueabihf \
    --platform=powerpc64le-unknown-linux-gnu \
    --platform=riscv64gc-unknown-linux-gnu \
    --platform=s390x-unknown-linux-gnu \
    --exclude-crate-path='gettext-sys#gettext-*.tar.xz' \
    vendor >/dev/null)
(cd "$stage" && cargo metadata --offline --locked --format-version 1 >/dev/null)

tar -C "$out" -cJf "$out/myna-config_$upstream.orig.tar.xz" \
    --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$commit_time" \
    "myna-config-$upstream"

cp -a "$here/debian" "$stage/debian"
# Noble's default rustc is 1.75, below the vendored closure's 1.85; its
# versioned toolchain from noble-updates is what debian/rules puts on PATH.
if [ "$series" = noble ]; then
    sed -i -e 's/^\( *\)cargo (>= [0-9.]*),/\1cargo-1.91,/' \
        -e 's/^\( *\)rustc (>= [0-9.]*),/\1rustc-1.91,/' "$stage/debian/control"
    if ! grep -q '^ *cargo-1.91,' "$stage/debian/control" || ! grep -q '^ *rustc-1.91,' "$stage/debian/control"; then
        echo "error: could not name the versioned Rust toolchain in debian/control" >&2
        exit 1
    fi
fi
# A release relabels its own entry for the series and PPA; a snapshot is not
# that release, so it gets an entry of its own above it.
case $upstream in
*+git*)
    python3 "$here/snapshot-changelog.py" "$root" "$version" "$series" \
        < "$here/debian/changelog" > "$stage/debian/changelog"
    ;;
*)
    sed -i "1s/($changelog_version) [a-z-]*;/($version) $series;/" "$stage/debian/changelog"
    ;;
esac
python3 "$here/vendor-copyright.py" "$stage/vendor" "$here/debian/copyright.in" > "$stage/debian/copyright"
rm "$stage/debian/copyright.in"

echo "$stage"
