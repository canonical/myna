# Preface

What the workshops build from and how each input is pinned, so the dev and CI environment only changes in a commit. Read before adding a tool, an SDK or a download to `.workshop/`, or when a workshop rebuilds unexpectedly.

Read the top-level `.kb/agents.md` file before continuing below.


# Overview

Every input to a workshop is named by version and checked by sha256, or frozen by date: the Ubuntu archive at one snapshot.ubuntu.com timestamp, the Rust toolchain and the gate tools by release, crates and wheels by `Cargo.lock` and `uv.lock`. The archive moves weekly through `.github/workflows/bump-pins.yml`, which commits the new timestamp and fast-forwards main only when CI passes on it. Everything else moves when someone edits it.


# Important

- No store SDKs. Workshop cannot pin a store SDK's revision, so a store release silently rebuilds every layer above it. Write an in-project SDK instead.
- Every download in a hook names a version and verifies a sha256. Never install from a branch, `latest`, or a piped installer script.
- `project-pins` stays first in every workshop. Workshop hashes each layer together with every layer below it, so moving the snapshot rebuilds everything and nothing above it can install from a different archive.
- A hook sees only its own SDK directory, so a pin cannot be read from the tree. Where a pin must match a file in the tree, `make check` compares them (the Rust toolchain, against `client/rust-toolchain.toml`).
- Workshop itself floats, locally and in CI alike, so both always run the same release. Pinning only CI would let the two disagree; pinning both means hand bumps of a pre-1.0 tool. The accepted cost: a Workshop format change rebuilds every layer once.
- Moving to a new Ubuntu release is a human decision: change `base:` in the workshop definitions. The weekly bump only follows updates within the current release.


# Architecture

## What cannot be pinned

The base image. Workshop takes `ubuntu@26.04` and LXD refreshes the image under it. The snapshot pin brings the image's packages to the snapshot (including downgrades), so the result does not depend on the image. But a new image fingerprint still changes every layer hash, so the stack rebuilds anyway, with identical contents.

## The CI apt cache

A CI runner shares no layers, so every workshop launch fetches the whole apt set from snapshot.ubuntu.com, which is slow and uneven (4 to 28 minutes for the same launch) and answers the odd 500. `.github/actions/apt-cache` runs apt-cacher-ng on the runner with its cache in the Actions cache, and writes `.workshop/pins/apt-proxy`; finding that file, the pins hook switches the sources to plain http, which the proxy can cache, and routes apt through it while it answers. Without the file the sources stay https and nothing changes locally. The proxy needs the `Remap-snapshot` line in its config: an apt-cacher-ng without it fetches the http URL, follows the redirect itself and serves by-hash indexes that fail their hash check.

## The CI snap cache

launch-workshop installs LXD and Workshop from api.snapcraft.io, which goes down for hours. `.github/actions/snaps` installs them first: from a fresh download when the store answers, else from the newest set in the Actions cache, saved whenever a download brings new revisions. launch-workshop then skips its own installs, so the action must also do what those skip: track `6/stable` on LXD (a file install tracks no channel, and launch-workshop refreshes any other) and open the runner firewall to `workshopbr0`. Nothing else in a launch asks the store: the system SDK ships in Workshop, and the base image comes from cloud-images.ubuntu.com.
