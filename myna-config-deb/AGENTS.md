# Preface

Read this file before touching the Debian packaging of Myna Settings, its version scheme, or the PPA upload.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

This directory turns `client/myna-core` and `client/myna-config` into the `myna-config` source package. The application must run unconfined because it drives snapd and escalates through polkit, which the `myna` snap cannot do, so it is the one piece of Myna shipped as a deb. `README.md` here is the human build recipe; this file carries what an agent must not get wrong.

# Important

- `build-source.sh` reads HEAD, not the working tree. Commit before staging or the tarball silently lacks the change.
- The orig tarball is reproducible and vendors every crates.io dependency, so the build runs offline. `debian/copyright` is generated from the vendor tree by `vendor-copyright.py`; edit `debian/copyright.in`, never the generated file.
- Versioning: `debian/changelog` holds the release version. Untagged commits build as `<upstream>~git<date>.<sha>`, tag `v<upstream>` for the release. `PPA=N` appends `~ppaN`, `SERIES=<name>` retargets a series and inserts `~<release>` so older series sort below newer ones. Do not hand-edit the changelog to fake a snapshot.
- The unshare sbuild chroot unpacks under `$TMPDIR`. On a tmpfs `/tmp` a debuginfo build fills it; use `TMPDIR=/var/tmp`. Aborted builds leave multi-gigabyte directories there.
- The binary package is `Architecture: amd64`. Nothing has been tested on another architecture, and resolute's ppc64el build segfaulted in the GTK widget tests. Widen it only after someone runs the application there.
- Keep the source lintian-clean. Run `lintian` on the staged source before uploading and fix tags rather than overriding them.
- The GSettings schema and the desktop entry are installed by this package, not by the snap. Changing either in `client/` changes what this deb ships.
- The effective MSRV is set by gtk4 0.11 and libadwaita 0.9 in `Cargo.lock`, not by the `rustc (>= 1.75)` pin in `debian/control` or the workspace `rust-version`. Both pins understate it; older series fail on the toolchain, not on the packaging.
- The autopkgtest in `debian/tests/` is the only CI that exercises the installed binary. Extend it when the CLI surface changes.

# Directory

- `build-source.sh` - Stages the orig tarball and debianised tree into `target/deb/`.
- `vendor-copyright.py` - Generates `debian/copyright` from the vendored crates.
- `debian/` - Packaging: `rules` builds offline with `--locked`, `install` places the schema and desktop entry, `tests/` is the autopkgtest.
