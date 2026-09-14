# myna-config deb

The Debian source package for Myna Settings (`client/myna-config`). It ships
separately from the `myna` snap because it drives snapd and runs `snap` as
root through polkit, neither of which a confined snap can do.

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

`build-source.sh` reads HEAD, not the working tree. It stages a two-crate
workspace (`myna-core`, `myna-config`, `client/data`), vendors the crates.io
dependencies for the Ubuntu build targets, and writes a reproducible
`.orig.tar.xz`. `debian/copyright` is generated from the vendored crates by
`vendor-copyright.py`; `debian/copyright.in` is the hand-written header.

## Versions

`debian/changelog` carries the release version. Untagged commits build as
`<upstream>~git<date>.<sha>`, which sorts below the release and gives every
commit its own orig tarball. Tag `v<upstream>` to build the release itself.
Uploads to a PPA take a `~ppaN` suffix on top; never commit that:

    PPA=1 make build-deb-source                   # stonking, ...-0ubuntu1~ppa1
    PPA=1 SERIES=resolute make build-deb-source   # ...-0ubuntu1~26.04~ppa1

One PPA (`ppa:charles05/myna-config`) carries every series; the `~<release>`
tag keeps an older series' build below a newer one's.
