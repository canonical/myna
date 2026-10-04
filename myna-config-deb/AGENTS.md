# Preface

Read this file before touching the Debian packaging of Myna Settings, its version scheme, or the PPA upload.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

This directory turns `client/myna-core` and `client/myna-config` into the `myna-config` source package. The application must run unconfined because it drives snapd and escalates through polkit, which the `myna` snap cannot do, so it is the one piece of Myna shipped as a deb. `README.md` here is the human build recipe; this file carries what an agent must not get wrong.

# Important

- `build-source.sh` reads HEAD, not the working tree. Commit before staging or the tarball silently lacks the change.
- The orig tarball is reproducible and vendors every crates.io dependency, so the build runs offline. `debian/copyright` is generated from the vendor tree by `vendor-copyright.py`; edit `debian/copyright.in`, never the generated file.
- Versioning: the upstream version is `dev/version.sh`'s, shared with the snaps: `X.Y.Z` on the tag `vX.Y.Z`, where it must equal the changelog's, and `X.Y.Z+git<n>.<sha>` past it. A snapshot sorts by commits since the tag, which rises only along one history, so the PPA is public and monotonic only if it ships main: `PPA=N` refuses a HEAD that is not on `origin/main`. `PPA=N` appends `~ppaN`, `SERIES=<name>` retargets a series and inserts `~<release>` so older series sort below newer ones. Do not hand-edit the changelog to fake a snapshot.
- The unshare sbuild chroot unpacks under `$TMPDIR`. On a tmpfs `/tmp` a debuginfo build fills it; use `TMPDIR=/var/tmp`. Aborted builds leave multi-gigabyte directories there.
- The binary package is `Architecture: amd64`. Nothing has been tested on another architecture, and resolute's ppc64el build segfaulted in the GTK widget tests. Widen it only after someone runs the application there.
- Keep the source lintian-clean. Run `lintian` on the staged source before uploading and fix tags rather than overriding them.
- The GSettings schema, the desktop entry and the AppStream metainfo are installed by this package, not by the snap. Changing any of them in `client/` changes what this deb ships.
- The vendored closure needs Rust 1.85, which the workspace `rust-version` and `debian/control` state. gtk-rs is held at 0.21 (gtk4 0.10) because 0.22 needs 1.92 and noble's newest toolchain is `rustc-1.91`. Noble's default `rustc` is 1.75, so `build-source.sh` swaps in the versioned `rustc-1.91`/`cargo-1.91` for `SERIES=noble` and `debian/rules` puts its `bin/` first on `PATH`.
- `.github/workflows/deb.yml` sbuilds every series the PPA carries on any push that changes what `build-source.sh` packs. Its path filter is that script's input list; a new input added here must be added there, or CI stops seeing its changes.
- The autopkgtest in `debian/tests/` is the only CI that exercises the installed binary. Extend it when the CLI surface changes. Launchpad PPA builds never run it; run it locally with sbuild's `--run-autopkgtest`.
- The myna-shell GNOME Shell extension ships at `/usr/share/gnome/gnome-shell/extensions/myna-shell@canonical.com` on every series. gnome-shell takes the first copy of a uuid in `XDG_DATA_DIRS` order (user dir first), and `/usr/share/gnome` precedes `/usr/share` in every GNOME session, so on Stonking it shadows `gnome-shell-ubuntu-extensions`' copy with no file overlap: no divert, no Breaks/Replaces. Never move it to `/usr/share/gnome-shell/extensions`, which would overlap that package's files.
- The shadow copy registers first even when gnome-shell refuses to run it, so a `shell-version` lacking the running major hides the indicator rather than falling back to Ubuntu's copy. Exposure is a release upgrade (the PPA is disabled, our old copy stays). Mitigations: add the next major to `extensions/myna-shell/metadata.json` only once `make test-extension-next` is green against it; `build-source.sh` runs `dev/check-shell-version.sh`, which fails when the target series' gnome-shell major is missing and warns for the devel series; when myna-shell lands upstream for the series after Stonking, ask for `Breaks: myna-config (<< X)` there.
- Dev override for the extension: `~/.local/share/gnome-shell/extensions/<uuid>` wins on Noble and Resolute, but the Ubuntu session on Stonking skips a user copy of a session-mode uuid. Use `/usr/share/ubuntu/gnome-shell/extensions/<uuid>` there (first in `XDG_DATA_DIRS`, owned by no package). A symlink at the old `/usr/share/gnome-shell/extensions/<uuid>` path is shadowed once the deb is installed.

# Directory

- `build-source.sh` - Stages the orig tarball and debianised tree into `target/deb/`, the extension included (from HEAD, through `dev/stage-extension.sh`).
- `vendor-copyright.py` - Generates `debian/copyright` from the vendored crates.
- `debian/` - Packaging: `rules` builds offline with `--locked`, `install` places the schema and icons, `rules` installs the catalogs and the translated desktop entry and metainfo, `tests/` is the autopkgtest.
