SHELL := /bin/bash
.DEFAULT_GOAL := help

# Target naming. Component-scoped targets are `<verb>-<component>` (fmt, lint,
# test, cov, build, mutate over client / server / extension / snaps), with an
# optional third part for a sub-suite (test-client-gated). Cross-cutting steps
# that span every component keep a single word (check, test, coverage, corpus,
# exercise, deadcode). The five gates at the top are the CI jobs, one each:
# CI invokes them and nothing else, so a gate added here is a gate CI enforces
# and there is no second list in a workflow to keep in step.

# The one list to extend for a new inference backend. Everything per-snap
# below (fetch command, packaged name, snap-*/bench-* targets) is derived from
# it; only a backend that breaks the conventions needs an explicit line.
#
# TODO(charles): not the only list. dev/bench.yaml (sweep targets),
# dev/lint-packages.sh (SNAP_DIRS) and tests/spread/*/task.yaml each name the
# backends they cover; a new one has to be added there by hand, and nothing
# fails when it is forgotten. Derive them from here, or check them against it.
BACKENDS := whisper parakeet nemotron qwen sherpa funasr audio8 fake
SNAPS := $(BACKENDS) myna

# Conventions, with the exceptions stated once each:
#
# Packaged snap name is `myna-<backend>`. The directory and every target are
# keyed on the short name; the store name is what targets that talk to an
# *installed* snap (sockets, services, sweeps) need, so map it here rather
# than making the caller remember which spelling a given target wants.
SNAPNAME_fake := myna-fake-backend
SNAPNAME_myna := myna
$(foreach s,$(SNAPS),$(eval SNAPNAME_$(s) ?= myna-$(s)))

# Model fetch is the snap's own dev/download-models.sh when it has one, and
# nothing when it carries no weights (myna, fake). Exceptions: parakeet's
# takes the encoder to stage; the two ONNX backends are fetched by repo-level
# scripts, and audio8's weights are CC-BY-NC-4.0 (non-commercial).
FETCH_parakeet  = cd parakeet-snap && ./dev/download-models.sh $(PARAKEET_ENCODER)
FETCH_funasr   := uv run ./dev/fetch_funasr_model.py --target ./funasr-snap/components/model-sensevoice-onnx
FETCH_audio8   := uv run ./dev/fetch_audio8_model.py --profile snap --target ./audio8-snap/components/model-audio8-onnx --accept-license "CC-BY-NC-4.0"
$(foreach s,$(SNAPS),$(eval FETCH_$(s) ?= $(if $(wildcard $(s)-snap/dev/download-models.sh),cd $(s)-snap && ./dev/download-models.sh)))

# Which encoder snap-parakeet stages; snap-parakeet-maxstack overrides it.
PARAKEET_ENCODER ?= base

BRANCH := $(shell git branch --show-current)

# Everything below `test`, `coverage` and `check` runs inside the canonical
# Workshop environment (.workshop/myna.yaml, .workshop/myna-shell.yaml): the
# same actions CI runs, so green here is green there.
WS := workshop run myna
WS_SHELL := workshop run myna-shell

.PHONY: help
help: ## List targets, grouped as in this file
	@awk 'BEGIN {FS = ":.*## "} \
		/^##@ / {printf "\n\033[1m%s\033[0m\n", substr($$0, 5); next} \
		/^[a-zA-Z0-9_.%-]+:.*## / {printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2}' \
		$(MAKEFILE_LIST)
	@echo
	@for s in $(SNAPS); do printf "  \033[36m%-24s\033[0m %s\n" "snap-$$s" "Build the $$s snap (fetch models, stage, snapcraft pack)"; done

##@ Gates (one per CI job; `preflight` is all of them)

.PHONY: preflight
preflight: check test coverage ## Everything CI blocks a merge on: check + test + coverage

.PHONY: check
check: lint-client lint-server lint-snaps lint-shell lint-workflows lint-client-deps i18n-check ## Static gates (CI `static` job)

.PHONY: test
test: test-client test-server test-extension ## Every blocking suite (CI `workshop` + `extension` jobs)

# The measured suites are `test` under instrumentation, plus the use-case
# exercise and the reports built on the merged exports. The patch gate at the
# end is the only blocking part; the project-level numbers are informational.
.PHONY: coverage
coverage: cov-client cov-server cov-extension exercise deadcode cov-patch ## Coverage + dead-code report + patch gate (CI `coverage` job)

.PHONY: spread
spread: spread-build ## Confined e2e suite in a local KVM VM (CI `spread` job; needs prebuilt snaps)
	.cache/spread/spread $(SPREAD_FLAGS) qemu:ubuntu-24.04-64:tests/spread/

# Every snap in one go. Kept serial even under `make -j`: the per-snap builds
# each want the whole machine (snapcraft's build VM/container, multi-GB model
# fetches) and interleaving them thrashes rather than parallelises.
#
# Budget ~60 GiB in the LXD `default` pool for a full run, or expect to call
# `clean-build-containers` partway through - see that target for why.
.NOTPARALLEL: snaps
.PHONY: snaps
snaps: $(SNAPS:%=snap-%) ## Build every snap (all the snap-* targets), in order

##@ Format (writes) and lint (checks)

.PHONY: fmt-client
fmt-client: ## Format the Rust workspace in place (cargo fmt)
	$(WS) fmt

.PHONY: fmt-server
fmt-server: ## Format the Python tree in place (ruff format: server + dev/)
	$(WS) py-fmt

.PHONY: lint-client
lint-client: ## Rust format check + clippy with warnings as errors
	$(WS) fmt-check
	$(WS) lint

.PHONY: lint-server
lint-server: ## Python ruff check + format check + mypy on the contract package
	$(WS) py-lint
	$(WS) py-types

.PHONY: lint-client-deps
lint-client-deps: ## Unused Cargo dependencies (machete) + ban/licence policy (deny)
	$(WS) machete
	$(WS) deny

.PHONY: lint-snaps
lint-snaps: ## Validate snap engine/runtime/model manifests with modelctl lint-package
	./dev/lint-packages.sh

.PHONY: lint-shell
lint-shell: ## shellcheck every dev, snap and hook script
	$(WS) shell-lint

.PHONY: lint-workflows
lint-workflows: ## actionlint the GitHub workflows
	$(WS) workflow-lint

.PHONY: i18n
i18n: ## Regenerate the translation templates (po/*.pot for myna-desktop + myna-orchestrator)
	$(WS) i18n

.PHONY: i18n-check
i18n-check: ## Fail if a committed .pot is stale against the sources it lists
	$(WS) i18n-check

##@ Test

.PHONY: test-client
test-client: test-client-hermetic test-client-gated test-client-ui ## Every Rust suite: hermetic + gated hardware + renderer smoke

.PHONY: test-client-hermetic
test-client-hermetic: ## cargo test --workspace, no services needed (workshop: test)
	$(WS) test

.PHONY: test-client-gated
test-client-gated: ## Env-gated PipeWire/IBus/D-Bus suites with private services stood up (workshop: test-gated)
	$(WS) test-gated

.PHONY: test-client-ui
test-client-ui: ## HUD renderer paints a wave under xvfb (workshop: ui-check)
	$(WS) ui-check

.PHONY: test-server
test-server: ## Python offline suite (workshop: py-test)
	$(WS) py-test

# Its own workshop, not `myna`: the Shell version a test can reach comes from
# the workshop's base, and the extension targets a newer one than the core24
# snap does. See .workshop/myna-shell.yaml.
.PHONY: test-extension
test-extension: ## GNOME Shell extension suites, incl. the headless-Shell presentation check (workshop myna-shell: gjs-test)
	$(WS_SHELL) gjs-test

.PHONY: test-extension-next
test-extension-next: ## The same suites against the NEXT GNOME Shell, in a throwaway LXD container (CI: non-blocking)
	extensions/myna-shell/test/next-shell.sh

##@ Coverage and mutation

.PHONY: cov-client
cov-client: ## Rust coverage: hermetic + gated suites, HTML/lcov/Cobertura (workshop: cov)
	$(WS) cov

.PHONY: cov-server
cov-server: ## Python branch coverage with per-test contexts (workshop: py-cov)
	$(WS) py-cov

.PHONY: cov-extension
cov-extension: ## GJS extension coverage (workshop: gjs-cov)
	$(WS) gjs-cov

.PHONY: corpus
corpus: ## Provision the english-speech corpus tier the exercise dictates from (idempotent)
	$(WS) corpus

# Prerequisite: cov-client and cov-server raw data on disk. dev/exercise.sh
# runs the suites itself when it is missing, so `make exercise` alone is
# correct, just slower than running after the two cov targets.
.PHONY: exercise
exercise: corpus ## Real use-cases under instrumentation, merged with the suites into per-language exports
	$(WS) exercise

# Reads the exports the three targets above leave on disk and fails loud when
# one is missing; it does not rerun them. `make coverage` is the from-scratch
# path. Stale exports are called out in the report itself.
.PHONY: deadcode
deadcode: ## Populations (test-covered / use-case-only / never-executed) + dead-code digest from the last exports
	$(WS) deadcode

# The blocking gate: 80% of changed coverable lines against COV_BASE, 5-line
# floor. Reads the merged exports `exercise` writes. Exit 2 = below threshold.
COV_BASE ?= origin/main
.PHONY: cov-patch
cov-patch: ## Patch-coverage gate on the lines this branch changes (COV_BASE=origin/main)
	$(WS) patch-cov --base $(COV_BASE)

# Mutation testing is scoped on purpose: a whole-workspace run is hours, a
# crate or a module is minutes. Use it to grade a suite you have just written
# or that a bug slipped past, not as a routine gate.
#   make mutate-client MUTATE='-p myna-orchestrator -f src/session.rs'
#   make mutate-server MUTATE='myna.core.session*'
MUTATE ?=
.PHONY: mutate-client
mutate-client: ## cargo-mutants over the Rust workspace, scoped by MUTATE (cargo-mutants args)
	$(WS) mutants $(MUTATE)

.PHONY: mutate-server
mutate-server: ## mutmut over the Python package, scoped by MUTATE (mutant name globs)
	$(WS) py-mutants $(MUTATE)

##@ Build

.PHONY: build-client
build-client: ## Build the Rust client workspace (release, on the host)
	cd client && cargo build --release

# The host venv, for editors and the in-tree bench scripts. The workshop has
# its own (shadowing this one with a mount), so nothing under `test` or
# `coverage` needs this; and a snapcraft build container that mounts the tree
# does NOT shadow it, so a venv it creates breaks this one. If host `uv run`
# starts re-resolving the interpreter, `rm -rf server/.venv` and sync again.
.PHONY: venv-server
venv-server: ## Sync the host Python venv for the server (editor tooling, bench scripts)
	cd server && uv sync

# The extension is not in the snap: gnome-shell only loads extensions from the
# host's own search path, so until it ships as a deb (T74) the delivery is a
# tarball the user unpacks into their extensions dir. The archive's single top
# level directory is the UUID gnome-shell keys the extension on - read out of
# metadata.json rather than repeated here, so a UUID change cannot produce a
# tarball that silently fails to load.
EXTENSION_DIR := extensions/myna-shell

.PHONY: build-extension
build-extension: ## Pack extensions/myna-shell into target/myna-shell-<rev>.tar.gz for hand-install
	@uuid=$$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["uuid"])' \
		$(EXTENSION_DIR)/metadata.json); \
	rev=$$(git describe --always --dirty --abbrev=7); \
	stage=target/extension-stage; \
	tarball=target/myna-shell-$$rev.tar.gz; \
	rm -rf $$stage; mkdir -p $$stage/$$uuid target; \
	cp $(EXTENSION_DIR)/*.js $(EXTENSION_DIR)/metadata.json $(EXTENSION_DIR)/README.md \
		$$stage/$$uuid/; \
	tar czf $$tarball -C $$stage \
		--sort=name --owner=0 --group=0 --numeric-owner \
		--mtime=@$$(git log -1 --format=%ct) $$uuid; \
	rm -rf $$stage; \
	echo; \
	echo "  $$tarball"; \
	echo; \
	echo "  install/upgrade on the target machine (the rm is what makes it an"; \
	echo "  upgrade rather than an overlay - unpacking alone leaves files that"; \
	echo "  a newer revision has deleted):"; \
	echo; \
	echo "    ext=~/.local/share/gnome-shell/extensions"; \
	echo "    rm -rf \$$ext/$$uuid"; \
	echo "    mkdir -p \$$ext && tar -xzf $$(basename $$tarball) -C \$$ext"; \
	echo "    gnome-extensions enable $$uuid"; \
	echo; \
	echo "  then log out and back in - gnome-shell does not hot-reload extension JS."

# The client settings store is GSettings (com.canonical.Myna.Dictation), so an
# *unpackaged* build needs the schema on the host to read or write anything -
# the snap carries its own copy, and the gnome-shell-extension deb will carry
# the host's once it exists (T74). Until then this is that install.
.PHONY: install-schema
install-schema: ## Install the client GSettings schema on the host (needs sudo)
	sudo install -Dm644 client/data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml \
		/usr/share/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml
	sudo glib-compile-schemas /usr/share/glib-2.0/schemas
	@echo "installed com.canonical.Myna.Dictation; read it with: GSETTINGS_BACKEND=keyfile myna-desktop --status"

##@ Snaps

define snap_rule
.PHONY: snap-$(1)
snap-$(1):
	$$(FETCH_$(1))
	cd $(1)-snap && ./dev/prepare.sh && snapcraft pack
endef
$(foreach s,$(SNAPS),$(eval $(call snap_rule,$(s))))

# Same snap, optimized encoder. The encoder is built once into the model cache
# (parakeet-maxstack-encoder) and staged from there.
.PHONY: snap-parakeet-maxstack
snap-parakeet-maxstack: ## Build the parakeet snap with the maxstack encoder
	$(MAKE) snap-parakeet PARAKEET_ENCODER=maxstack

.PHONY: parakeet-maxstack-encoder
parakeet-maxstack-encoder: ## Build the maxstack encoder into the model cache (input to snap-parakeet-maxstack)
	./dev/parakeet/build-maxstack.sh

##@ Benchmark

.PHONY: build-bench
build-bench: ## Build the standalone myna-bench.pyz zipapp (what testers download; every bench-* runs it)
	./dev/build-bench.sh

# BENCH_LABEL_SUFFIX=maxstack tags every label <snap>+maxstack so two builds of
# one snap can share results/bench.jsonl (the summary dedups by label).
BENCH_LABEL_SUFFIX ?=
BENCH_LABEL_ARGS = $(if $(BENCH_LABEL_SUFFIX),--label-suffix $(BENCH_LABEL_SUFFIX))

# Every bench target below runs myna-bench.pyz, the artefact testers download,
# not a second path into the same package. Running it here is what keeps the two
# honest: a zipapp nobody uses until a tester does is a zipapp that breaks in
# front of a tester. dev/bench.yaml is the same shape as the bench.yaml they
# write; only the artefact paths differ (globs into this tree).
#
# It also takes the venv out of the sudo path. `sudo .venv/bin/python` left
# root-owned __pycache__ dirs behind that broke every later `uv run`; the zipapp
# is self-contained (websockets, psutil, pyyaml vendored) and writes nothing here.
BENCH_PYZ = myna-bench.pyz
BENCH = python3 $(BENCH_PYZ)
BENCH_ROOT = sudo python3 $(BENCH_PYZ)
BENCH_CONFIG = dev/bench.yaml

.PHONY: bench-check
bench-check: build-bench ## Report whether this machine is fit to benchmark on (governor, load, competing servers)
	$(BENCH) check --sweep

.PHONY: bench-plan
bench-plan: build-bench ## Print the sweep matrix without installing anything (no root)
	$(BENCH) plan --config $(BENCH_CONFIG) $(BENCH_LABEL_ARGS)

# Installs and purges real snaps as root (snap remove --purge between
# targets): this modifies system state, run it yourself when ready.
.PHONY: bench-run
bench-run: build-bench bench-corpus ## Full snap matrix sweep (sudo: installs/removes snaps); writes results/bench.jsonl
	$(BENCH_ROOT) run --config $(BENCH_CONFIG) $(BENCH_LABEL_ARGS)

# --keep-results: unlike bench-run (a full sweep, meant to start clean),
# bench-run-<snap> exists to be called once per snap across separate
# invocations to build up one comparison. The runner resets results/bench.jsonl
# on every run by default, which would make each scoped run erase the last.
# Safe to re-run the same snap too: the summary dedups by (label, clip),
# newest wins.
bench-run-%: build-bench bench-corpus ## Sweep scoped to one snap (bench-run-<snap>, e.g. bench-run-whisper)
	$(BENCH_ROOT) run --config $(BENCH_CONFIG) --only $(SNAPNAME_$*) --keep-results $(BENCH_LABEL_ARGS)

.PHONY: bench-aggregate
bench-aggregate: build-bench ## Re-print the comparison table from the last sweep
	$(BENCH) summarize --by-category --in results/bench.jsonl

# Fold a submission back in. The leaderboard is one tracked file; re-running a
# machine replaces that machine's rows rather than doubling them.
.PHONY: bench-merge
bench-merge: build-bench ## Merge submissions into the leaderboard (make bench-merge SUBMISSIONS="a.jsonl b.jsonl")
	$(BENCH) merge $(SUBMISSIONS) --leaderboard results/leaderboard.jsonl
	$(BENCH) summarize --in results/leaderboard.jsonl

# A whole LibriSpeech chapter concatenated in reading order (real speech, not
# synthetic), category "long-form": the per-utterance tiers are all a few
# seconds each and never exercise rolling-window/buffer invariants a
# streaming adapter only hits minutes into a session.
#
# bench-corpus regenerates dev/bench.yaml's own manifest (manifest-balanced.json,
# same -n 80 --select balanced that produced the committed-shape tier) with the
# long-form clip folded in as one more entry, so `make bench-run` sweeps it
# automatically. bench-corpus-long is the standalone single-clip manifest for
# ad hoc bench-long-<snap> runs against one already-running snap.
.PHONY: bench-corpus
bench-corpus: build-bench ## Regenerate the sweep's corpus (manifest-balanced.json) with the long-form clip included
	$(BENCH) download-corpus --out corpus/english --cache .cache/librispeech \
		-n 80 --select balanced --manifest-name manifest-balanced.json --long-form-minutes 5

.PHONY: bench-corpus-long
bench-corpus-long: build-bench ## (Re)generate a standalone ~5min long-form clip (corpus/english/manifest-long.json)
	$(BENCH) download-corpus --out corpus/english --cache .cache/librispeech \
		-n 0 --manifest-name manifest-long.json --long-form-minutes 5

# e.g. `make bench-long-whisper`: assumes the snap is already installed and
# its server started (this only scores its socket, it does not install/purge
# like bench-run does). --realtime: a long clip fed flat out can outrun a
# backend's websocket keepalive, which reads as a 100% WER model failure.
bench-long-%: build-bench ## Run the long-form clip against an already-running <snap> (bench-long-<snap>)
	$(BENCH) bench --realtime \
		--socket /var/snap/$(SNAPNAME_$*)/common/run/ubustt.sock \
		--manifest corpus/english/manifest-long.json \
		--out results/bench.jsonl --label $(SNAPNAME_$*)/long-form

##@ Spread (local, confined e2e; needs /dev/kvm)

# Prebuilt snaps must exist first: `make snap-myna snap-fake snap-whisper`.
# The first run primes the qemu image (~1 GB) and builds spread at the commit
# pinned in .github/workflows/spread.yml, same as CI. SPREAD_FLAGS=-debug
# keeps the VM around after a failure.
SPREAD_FLAGS ?=

.PHONY: spread-image
spread-image: ## Prime the qemu image for local spread runs (one-time, ~1 GB)
	./dev/spread-image.sh

.PHONY: spread-build
spread-build: spread-image ## Build spread at the pinned commit (input to every spread run)
	./dev/spread-build.sh

# One suite by its directory name under tests/spread/: adapter-smoke (real
# whisper snap, batch + streaming), confined-e2e (fake backend), thread-pinning
# (real funasr snap, ORT affinity under confinement), control-socket (client
# snap, network-bind seccomp bind(2)).
spread-%: spread-build ## Run one suite: spread-<dir under tests/spread>, e.g. spread-confined-e2e
	.cache/spread/spread $(SPREAD_FLAGS) qemu:ubuntu-24.04-64:tests/spread/$*

##@ Remote CI (GitHub Actions, current branch)

.PHONY: ci
ci: ## Trigger the CI workflow on GitHub for the current branch
	gh workflow run ci.yml --ref $(BRANCH)

ci-%: ## Trigger another workflow: ci-snap, ci-spread, ci-audit, ci-codeql (.github/workflows/<name>.yml)
	gh workflow run $*.yml --ref $(BRANCH)

.PHONY: ci-watch
ci-watch: ## Watch the most recent GitHub Actions run on the current branch
	gh run watch $$(gh run list --branch $(BRANCH) --limit 1 --json databaseId --jq '.[0].databaseId')

.PHONY: audit
audit: ## Advisory audits, cargo audit + pip-audit (CI `audit` workflow, weekly)
	$(WS) audit

##@ Escape hatch

# Any workshop action not wrapped above, e.g. `make workshop-shell-version-probe`.
# See .workshop/myna.yaml. Anything CI calls gets a named target instead.
workshop-%: ## Run any workshop action directly (workshop-<action>)
	$(WS) $*

##@ Clean

.PHONY: clean-snaps
clean-snaps: ## Remove built snap/component artifacts and staged wheels/models
	rm -rf */wheels
	rm -f */*.snap */*.comp
	rm -rf */components/model-*

# snapcraft keeps one LXD build container per *project directory*, forever, and
# never reclaims them. Nine snaps is nine containers; two checkouts is
# eighteen. They live in the `default` storage pool, which is a fixed-size ZFS
# image shared with any long-lived dev containers, so a full `make snaps` can
# fill it - after which every build fails in about three seconds with
# `No space left on device` or `saving config file for the container failed`,
# neither of which names the real cause.
#
# The containers are caches: snapcraft recreates one on demand, paying only the
# copy from the base instance (which this deliberately keeps). Reclaim ours by
# the project-directory inode snapcraft names them after, so a parallel
# checkout's containers - and every non-snapcraft container - are left alone.
.PHONY: clean-build-containers
clean-build-containers: ## Delete this checkout's snapcraft LXD build containers (they are caches)
	@for d in $(SNAPS); do \
		inode=$$(stat -c '%i' "$$d-snap" 2>/dev/null) || continue; \
		name=$$(lxc list --project snapcraft -c n --format csv 2>/dev/null | grep -- "-$$inode$$") || continue; \
		echo "reclaiming $$name"; \
		lxc stop --project snapcraft -f "$$name" >/dev/null 2>&1 || true; \
		lxc delete --project snapcraft -f "$$name" || true; \
	done
	@lxc storage info default 2>/dev/null | grep -E 'space used|total space' || true

.PHONY: clean-coverage
clean-coverage: ## Remove every coverage, exercise and mutation output
	rm -rf client/target/coverage .coverage-work
	rm -rf server/htmlcov server/coverage-*.xml server/.coverage server/.coverage.* server/mutants
	rm -rf extensions/myna-shell/target/coverage client/mutants.out

.PHONY: clean
clean: clean-snaps clean-coverage ## clean-snaps + clean-coverage + Rust build, bench zipapp and extension tarball
	rm -rf client/target
	rm -f $(BENCH_PYZ)
	rm -rf target/extension-stage target/myna-shell-*.tar.gz
