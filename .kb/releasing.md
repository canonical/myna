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
- Try the commit as a store refresh before tagging it: `gh workflow run snaps.yml --ref main -f publish=true -f channel=edge/rc1` releases it to a branch that edge users never see and that expires after 30 days. A sideloaded snap does not exercise the refresh: it keeps no store components and gets no store-granted connections.
- Releases link the PPA and the store, never attach a `.snap` or `.deb`: a sideloaded snap is unasserted and never refreshes, a loose deb never updates. The install text is `.github/release-footer.md`, appended to every release's notes.


# Architecture

The steps, in order:

1. `make release-notes > notes.md` drafts the features and fixes since the last tag, internal scopes left out, each followed by the GitHub issues its commit body names (`#79`): a fix commit names its issue in the body, never with a `Fixes` trailer, which would close the issue on the push to `main` rather than on release. Drop a reference that is only a related mention. Cut it down to the highlights a user cares about; keep the full lists only where they help.
2. `dch -v X.Y.Z-0ubuntu1 -D <devel series>` in `myna-config-deb/`, with the highlights as its bullets. Commit it and push it to main.
3. `git tag -s --cleanup=verbatim -F notes.md vX.Y.Z`, then `git push origin vX.Y.Z`. `.github/workflows/release.yml` drafts the GitHub Release from the tag message and the footer.
4. The tag also runs `.github/workflows/snaps.yml`, which builds, smoke-tests and releases the five snaps to edge on amd64 and arm64 runners, once the `store-edge` environment's reviewer approves. `make publish-<snap> CHANNEL=latest/edge` from the tagged checkout is the fallback for amd64.
5. From the tagged checkout, the PPA uploads per series (`myna-config-deb/AGENTS.md`). The PPA builds arm64 from the same uploads.
6. Once the store and the PPA serve `X.Y.Z` on both architectures, `gh release edit vX.Y.Z --draft=false`.
7. Close each issue the notes say this release fixes, with a comment linking the release.
