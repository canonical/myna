#!/bin/sh
# Stage the myna-config Debian source package from the committed tree.
#
# The deb builds a two-crate workspace (myna-core, myna-config) cut out of
# client/, with its crates.io dependencies vendored into the orig tarball so
# the build runs offline. Output, under target/deb/ by default:
#
#   myna-config_<upstream>.orig.tar.xz    reproducible: git archive + cargo vendor,
#                                         mtimes pinned to the commit date
#   myna-config-<upstream>/               the unpacked source with debian/ applied,
#                                         ready for `sbuild` run from inside it
#
# <upstream> is the changelog's upstream version when HEAD carries the tag
# v<upstream>, and <upstream>~git<date>.<sha> otherwise. A snapshot therefore
# sorts below the release it precedes, and every commit gets its own orig
# tarball, so Launchpad never sees two different tarballs under one name.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out=${1:-$root/target/deb}

if [ -n "$(git -C "$root" status --porcelain -- client myna-config-deb)" ]; then
    echo "warning: uncommitted changes under client/ or myna-config-deb/ are not staged (git archive reads HEAD)" >&2
fi

changelog_version=$(dpkg-parsechangelog -l "$here/debian/changelog" -S Version)
upstream=${changelog_version%-*}
revision=${changelog_version##*-}
commit=$(git -C "$root" rev-parse --short=7 HEAD)
commit_time=$(git -C "$root" log -1 --format=%ct)
if ! git -C "$root" describe --tags --exact-match --match "v$upstream" >/dev/null 2>&1; then
    upstream="$upstream~git$(date -u -d "@$commit_time" +%Y%m%d).$commit"
fi
version="$upstream-$revision"
stage="$out/myna-config-$upstream"

rm -rf "$out"
mkdir -p "$stage"
git -C "$root" archive HEAD client/Cargo.toml client/Cargo.lock client/data client/myna-core client/myna-config \
    | tar -x -C "$stage" --strip-components=1

# Tests that read the repository (snapcraft.yaml, docs) have nothing to read here.
rm "$stage/myna-config/tests/snap_packaging.rs" "$stage/myna-config/tests/developer_entrypoints.rs"

members='members = ["myna-core", "myna-config"]'
if [ "$(grep -c '^members = ' "$stage/Cargo.toml")" != 1 ]; then
    echo "error: expected exactly one workspace members line in client/Cargo.toml" >&2
    exit 1
fi
sed -i "s/^members = .*/$members/" "$stage/Cargo.toml"

# Prunes Cargo.lock to the two members' closure; versions stay as locked.
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
sed -i "1s/($changelog_version)/($version)/" "$stage/debian/changelog"
python3 "$here/vendor-copyright.py" "$stage/vendor" "$here/debian/copyright.in" > "$stage/debian/copyright"
rm "$stage/debian/copyright.in"

echo "$stage"
