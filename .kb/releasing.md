# Preface

How to cut a release: the version number, the notes, the tag and what gets published from it. Read before tagging or publishing a release.

Read the top-level `.kb/agents.md` file before continuing below.


# Overview

A release is a signed annotated `vX.Y.Z` tag whose message is the release notes. Everything else follows from the tag: `dev/version.sh` versions every artifact from it (`.kb/versioning.md`), CI turns its message into a draft GitHub Release, the snaps and the PPA are uploaded by hand from the tagged checkout, and the draft is published once they carry the version. The GitHub Release is the one place a release is described; posts, store text and tickets link to it.


# Important

- Pre-1.0, bump the minor for any release with features and the patch only for a fixes-only release.
- `debian/changelog` must release `X.Y.Z` at the tagged commit, or `build-source.sh` refuses the tag. Its entry carries the same highlights.
- Tag with `--cleanup=verbatim`. The default cleanup strips lines starting with `#`, which takes the Markdown headings with it.
- Tag a commit on `origin/main`; the PPA refuses anything else.
- Releases link the PPA and the store, never attach a `.snap` or `.deb`: a sideloaded snap is unasserted and never refreshes, a loose deb never updates. The install text is `.github/release-footer.md`, appended to every release's notes.


# Architecture

The steps, in order:

1. `make release-notes > notes.md` drafts the features and fixes since the last tag, internal scopes left out. Cut it down to the highlights a user cares about; keep the full lists only where they help.
2. `dch -v X.Y.Z-0ubuntu1 -D <devel series>` in `myna-config-deb/`, with the highlights as its bullets. Commit it and push it to main.
3. `git tag -s --cleanup=verbatim -F notes.md vX.Y.Z`, then `git push origin vX.Y.Z`. `.github/workflows/release.yml` drafts the GitHub Release from the tag message and the footer.
4. From the tagged checkout: `make publish-<snap> CHANNEL=latest/edge` for each published snap, then the PPA uploads per series (`myna-config-deb/AGENTS.md`).
5. Once the store and the PPA serve `X.Y.Z`, `gh release edit vX.Y.Z --draft=false`.
