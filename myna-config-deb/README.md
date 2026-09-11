# myna-config deb

The Debian source package for Myna Settings (`client/myna-config`). It ships
separately from the `myna` snap because it drives snapd and runs `snap` as
root through polkit, neither of which a confined snap can do.

## Build

    make deb-source   # target/deb/: orig tarball + debianised source tree
    make deb          # the above, then sbuild in a clean chroot for the
                      # series named in debian/changelog

`build-source.sh` reads HEAD, not the working tree. It stages a two-crate
workspace (`myna-core`, `myna-config`, `client/data`), vendors the crates.io
dependencies for the Ubuntu build targets, and writes a reproducible
`.orig.tar.xz`. `debian/copyright` is generated from the vendored crates by
`vendor-copyright.py`; `debian/copyright.in` is the hand-written header.

## Versions

`debian/changelog` carries the release version. Untagged commits build as
`<upstream>~git<date>.<sha>`, which sorts below the release and gives every
commit its own orig tarball. Tag `v<upstream>` to build the release itself.
Uploads to a PPA take a `~ppaN` suffix on top; never commit that.
