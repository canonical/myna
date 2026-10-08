# myna-config deb

The Debian source package for Myna Settings (`client/myna-config`). It ships
separately from the `myna` snap because it drives snapd and runs `snap` as
root through polkit, neither of which a confined snap can do.

Besides the application it installs the GSettings schema the daemon reads,
the desktop entry, icons and AppStream metainfo, the polkit action, and the
myna-shell GNOME Shell extension
(`/usr/share/gnome/gnome-shell/extensions/myna-shell@canonical.com`).

## Build

    make build-deb-source   # target/deb/: orig tarball + debianised source tree
    make build-deb          # the above, then sbuild in a clean chroot for the
                            # series named in debian/changelog

The `.deb`, `.changes` and `.buildinfo` land in `target/deb/`, overriding any
`$build_dir` in the sbuild config. The next `make build-deb-source` wipes that
directory.

`make build-deb SBUILD_ARGS=...` passes flags through to sbuild. Two that matter on
a developer machine: the unshare chroot unpacks under `$TMPDIR` (default
`/tmp`), and a full-debuginfo build needs several GB there, so run with
`TMPDIR=/var/tmp` when `/tmp` is a tmpfs; and
`--chroot-setup-commands='...'` is where a mirror or apt proxy for the chroot
goes.

`build-source.sh` reads HEAD, not the working tree. It stages a three-crate
workspace (`myna-core`, `myna-platform`, `myna-config`, `client/data`), vendors the crates.io
dependencies for the Ubuntu build targets, and writes a reproducible
`.orig.tar.xz`. `debian/copyright` is generated from the vendored crates by
`vendor-copyright.py`; `debian/copyright.in` is the hand-written header.

## Versions

The upstream version is `dev/version.sh`'s, shared with the snaps. The tag
`vX.Y.Z` builds `X.Y.Z`, which must equal the `debian/changelog` version.
Commits past it build as `X.Y.Z+git<n>.<sha>`, `<n>` counting commits since
the tag, which sorts above the release and gives every commit its own orig
tarball.

`SERIES=<name>` retargets a series other than the changelog's and tags the
revision `~<release>`. Uploads to a PPA take a `~ppaN` suffix on top, and
`PPA=N` refuses a HEAD that is not on `origin/main`; never commit either:

    SERIES=noble make build-deb                   # local, ...-0ubuntu1~24.04
    PPA=1 make build-deb-source                   # stonking, ...-0ubuntu1~ppa1
    PPA=1 SERIES=resolute make build-deb-source   # ...-0ubuntu1~26.04~ppa1

One PPA (`ppa:canonical-desktop-team/myna`) carries every series; the
`~<release>` tag keeps an older series' build below a newer one's.
