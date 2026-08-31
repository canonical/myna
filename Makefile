SHELL := /bin/bash
.DEFAULT_GOAL := help

SNAPS := whisper parakeet nemotron qwen sherpa funasr audio8 myna fake

# Model fetch, one per snap; empty for the snaps that carry no weights (myna,
# fake). Five have their own dev/download-models.sh (whisper's fetches
# tiny/base/small, qwen's 0.6B + 1.7B); the two ONNX ones are fetched by
# repo-level scripts. audio8's weights are CC-BY-NC-4.0 (non-commercial).
FETCH_whisper  := cd whisper-snap && ./dev/download-models.sh
FETCH_parakeet := cd parakeet-snap && ./dev/download-models.sh
FETCH_nemotron := cd nemotron-snap && ./dev/download-models.sh
FETCH_qwen     := cd qwen-snap && ./dev/download-models.sh
FETCH_sherpa   := cd sherpa-snap && ./dev/download-models.sh
FETCH_funasr   := uv run ./dev/fetch_funasr_model.py --target ./funasr-snap/components/model-sensevoice-onnx
FETCH_audio8   := uv run ./dev/fetch_audio8_model.py --profile snap --target ./audio8-snap/components/model-audio8-onnx --accept-license "CC-BY-NC-4.0"

# Short name -> packaged snap name. The two differ everywhere: the directory and
# these targets are keyed on the adapter, while the snap itself is namespaced
# `myna-*` for the store. Targets that talk to an *installed* snap (sockets,
# services, matrix targets) need the packaged name, so map it once here rather
# than making the caller remember which spelling a given target wants.
SNAPNAME_whisper  := myna-whisper
SNAPNAME_parakeet := myna-parakeet
SNAPNAME_nemotron := myna-nemotron
SNAPNAME_qwen     := myna-qwen
SNAPNAME_sherpa   := myna-sherpa
SNAPNAME_funasr   := myna-funasr
SNAPNAME_audio8   := myna-audio8
SNAPNAME_myna     := myna
SNAPNAME_fake     := myna-fake-backend

BRANCH := $(shell git branch --show-current)

# ------------------------------------------------------------------------
# help
# ------------------------------------------------------------------------

.PHONY: help
help: ## List targets with descriptions
	@grep -E '^[a-zA-Z0-9_.%-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Per-snap targets (generated: fetch models, stage, snapcraft pack):"
	@for s in $(SNAPS); do printf "  \033[36m%-22s\033[0m %s\n" "snap-$$s" "Build the $$s snap"; done

# ------------------------------------------------------------------------
# snaps - fetch + prepare + pack (uniform across every snap)
# ------------------------------------------------------------------------

define snap_rule
.PHONY: snap-$(1)
snap-$(1):
	$$(FETCH_$(1))
	cd $(1)-snap && ./dev/prepare.sh && snapcraft pack
endef
$(foreach s,$(SNAPS),$(eval $(call snap_rule,$(s))))

# Every snap in one go. Kept serial even under `make -j`: the per-snap builds
# each want the whole machine (snapcraft's build VM/container, multi-GB model
# fetches) and interleaving them thrashes rather than parallelises.
#
# Budget ~60 GiB in the LXD `default` pool for a full run, or expect to call
# `clean-build-containers` partway through - see that target for why.
.NOTPARALLEL: snaps
.PHONY: snaps
snaps: $(SNAPS:%=snap-%) ## Build every snap (all the snap-* targets below), in order

.PHONY: lint-snaps
lint-snaps: ## Validate snap engine/runtime/model manifests with modelctl lint-package
	./dev/lint-packages.sh

# ------------------------------------------------------------------------
# client / server / bench
# ------------------------------------------------------------------------

.PHONY: client
client: ## Build the Rust client workspace (release)
	cd client && cargo build --release

# The client settings store is GSettings (com.canonical.Myna.Dictation), so an
# *unpackaged* build needs the schema on the host to read or write anything -
# the snap carries its own copy, and the gnome-shell-extension deb will carry
# the host's once it exists (T74). Until then this is that install.
.PHONY: install-schema
install-schema: ## Install the client GSettings schema on the host (needs sudo)
	sudo install -Dm644 client/data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml \
		/usr/share/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml
	sudo glib-compile-schemas /usr/share/glib-2.0/schemas
	@echo "installed com.canonical.Myna.Dictation; read it with: gsettings get com.canonical.Myna.Dictation streaming-mode"

.PHONY: i18n
i18n: ## Regenerate the myna-hud translation template (po/myna.pot)
	cd client/myna-hud && xgettext --from-code=UTF-8 --keyword=gettext --keyword=n_ \
		--add-comments=TRANSLATORS --output=po/myna.pot --files-from=po/POTFILES.in

.PHONY: server
server: ## Sync the Python server env
	cd server && uv sync

.PHONY: bench
bench: ## Build the standalone myna-bench.pyz zipapp (external distribution, not an in-repo run)
	./dev/build-bench.sh

# In-repo benchmark run: dev/matrix.py sideloads the snap/component files
# already sitting in each */-snap dir (no bench.yaml needed — see
# dev/matrix.yaml for the target list) and sweeps corpus/real. Distinct from
# `bench`/myna-bench.pyz above, which is the config-driven tool for testers
# without a checkout.
.PHONY: bench-plan
bench-plan: bench-corpus ## Print the matrix run plan without installing anything
	cd server && uv run python ../dev/matrix.py --config ../dev/matrix.yaml --dry-run

# Installs and purges real snaps as root (snap remove --purge between
# targets) — this modifies system state, run it yourself when ready.
.PHONY: bench-run
bench-run: bench-corpus ## Full snap matrix sweep (sudo: installs/removes snaps); writes results/bench.jsonl
	sudo server/.venv/bin/python dev/matrix.py --config dev/matrix.yaml

# --keep-results: unlike bench-run (a full sweep, meant to start clean),
# bench-run-<snap> exists to be called once per snap across separate
# invocations to build up one comparison — matrix.py resets results/bench.jsonl
# on every run by default, which would make each scoped run erase the last.
# Safe to re-run the same snap too: aggregate.py dedups by (label, clip),
# newest wins.
bench-run-%: bench-corpus ## Matrix sweep scoped to one snap (bench-run-<snap>, e.g. bench-run-whisper)
	sudo server/.venv/bin/python dev/matrix.py --config dev/matrix.yaml --only $(SNAPNAME_$*) --keep-results

.PHONY: bench-aggregate
bench-aggregate: ## Re-print the comparison table from the last matrix run
	cd server && uv run python ../dev/aggregate.py --by-category --in ../results/bench.jsonl

# A whole LibriSpeech chapter concatenated in reading order (real speech, not
# synthetic), category "long-form" — the per-utterance tiers are all a few
# seconds each and never exercise rolling-window/buffer invariants a
# streaming adapter only hits minutes into a session.
#
# bench-corpus regenerates dev/matrix.yaml's own manifest (manifest-balanced.json,
# same -n 80 --select balanced that produced the committed-shape tier) with the
# long-form clip folded in as one more entry, so `make bench-run` sweeps it
# automatically. bench-corpus-long is the standalone single-clip manifest for
# ad hoc bench-long-<snap> runs against one already-running snap.
.PHONY: bench-corpus
bench-corpus: ## Regenerate the matrix's corpus (manifest-balanced.json) with the long-form clip included
	cd server && uv run python ../dev/fetch_real_corpus.py \
		-n 80 --select balanced --manifest-name manifest-balanced.json --long-form-minutes 5

.PHONY: bench-corpus-long
bench-corpus-long: ## (Re)generate a standalone ~5min long-form clip (corpus/real/manifest-long.json)
	cd server && uv run python ../dev/fetch_real_corpus.py \
		-n 0 --manifest-name manifest-long.json --long-form-minutes 5

# e.g. `make bench-long-whisper` — assumes the snap is already installed and
# its server started (this only drives dev/bench.py against its socket, it
# does not install/purge like bench-run does).
bench-long-%: ## Run the long-form clip against an already-running <snap> (bench-long-<snap>)
	cd server && uv run python ../dev/bench.py \
		--socket /var/snap/$(SNAPNAME_$*)/common/run/ubustt.sock \
		--manifest ../corpus/real/manifest-long.json --label $(SNAPNAME_$*)/long-form

# ------------------------------------------------------------------------
# tests / lint / coverage — delegate to the canonical Workshop environment
# (.workshop/myna.yaml; same actions CI runs)
# ------------------------------------------------------------------------

.PHONY: test-client
test-client: ## Rust test suite (workshop: test)
	workshop run myna test

.PHONY: test-gated
test-gated: ## Rust env-gated hardware suites, services stood up (workshop: test-gated)
	workshop run myna test-gated

.PHONY: test-server
test-server: ## Python test suite (workshop: py-test)
	workshop run myna py-test

# Its own workshop, not `myna`: the Shell version a test can reach comes from
# the workshop's base, and the extension targets a newer one than the core24
# snap does. See .workshop/myna-shell.yaml.
.PHONY: test-extension
test-extension: ## GNOME Shell extension suites, incl. the headless-Shell presentation check (workshop myna-shell: gjs-test)
	workshop run myna-shell gjs-test

.PHONY: test-extension-next
test-extension-next: ## The same suites against the NEXT GNOME Shell, in a throwaway LXD container
	extensions/myna-shell/test/next-shell.sh

.PHONY: lint-client
lint-client: ## Rust lints as errors (workshop: lint)
	workshop run myna lint

.PHONY: ui-check
ui-check: ## Renderer UI smoke check under xvfb (workshop: ui-check)
	workshop run myna ui-check

.PHONY: lint-server
lint-server: ## Python lint + format check (workshop: py-lint)
	workshop run myna py-lint

.PHONY: fmt
fmt: ## Rust format check (workshop: fmt)
	workshop run myna fmt

.PHONY: cov
cov: ## Rust coverage (workshop: cov)
	workshop run myna cov

.PHONY: py-cov
py-cov: ## Python coverage (workshop: py-cov)
	workshop run myna py-cov

.PHONY: gjs-cov
gjs-cov: ## GJS extension coverage (workshop: gjs-cov)
	workshop run myna gjs-cov

# The one definition of the static gate battery: CI's `static` job runs
# `make check` and nothing else, so a gate added here is a gate CI enforces -
# there is no second list in the workflow to keep in step. Deliberately
# excludes lint-client/test-client/test-server, which are CI's separate
# `workshop` job (and `make lint-client` etc. locally).
.PHONY: check
check: fmt lint-server lint-snaps ## All static gates (CI's `static` job runs this)
	workshop run myna machete
	workshop run myna deny
	workshop run myna py-types
	workshop run myna shell-lint
	workshop run myna workflow-lint

# Catch-all: any workshop action not wrapped above, e.g. `make workshop-deadcode`,
# `make workshop-corpus`, `make workshop-exercise`. See .workshop/myna.yaml.
workshop-%: ## Run any workshop action directly (workshop-<action>)
	workshop run myna $*

# ------------------------------------------------------------------------
# spread (local, confined e2e)
#
# Needs KVM (/dev/kvm). Prebuilt snaps must exist first:
#   make snap-myna snap-fake snap-whisper
# `make spread` primes the qemu image (~1 GB, one-time) and builds spread at
# the commit pinned in .github/workflows/spread.yml (same as CI).
# ------------------------------------------------------------------------

.PHONY: spread
spread: ## Run the confined e2e suite locally (primes image + builds spread as needed)
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread qemu:ubuntu-24.04-64:tests/spread/

.PHONY: spread-smoke
spread-smoke: ## Run only adapter-smoke (real whisper snap, batch + streaming)
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread qemu:ubuntu-24.04-64:tests/spread/adapter-smoke

.PHONY: spread-e2e
spread-e2e: ## Run only confined-e2e (fake backend)
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread qemu:ubuntu-24.04-64:tests/spread/confined-e2e

.PHONY: spread-pinning
spread-pinning: ## Run only thread-pinning (real funasr snap, ORT affinity under confinement)
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread qemu:ubuntu-24.04-64:tests/spread/thread-pinning

.PHONY: spread-control
spread-control: ## Run only control-socket (client snap, network-bind seccomp bind(2))
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread qemu:ubuntu-24.04-64:tests/spread/control-socket

.PHONY: spread-debug
spread-debug: ## Debug adapter-smoke (keep the VM around after the run)
	./dev/spread-image.sh
	./dev/spread-build.sh
	.cache/spread/spread -debug qemu:ubuntu-24.04-64:tests/spread/adapter-smoke

.PHONY: spread-image
spread-image: ## Prime the qemu image for local spread runs (one-time, ~1 GB)
	./dev/spread-image.sh

# ------------------------------------------------------------------------
# remote CI (GitHub Actions, current branch)
# ------------------------------------------------------------------------

.PHONY: ci
ci: ## Trigger the CI workflow on GitHub for the current branch
	gh workflow run ci.yml --ref $(BRANCH)

.PHONY: snap-ci
snap-ci: ## Trigger the Snap workflow on GitHub for the current branch
	gh workflow run snap.yml --ref $(BRANCH)

.PHONY: spread-ci
spread-ci: ## Trigger the Spread (confined e2e) workflow on GitHub for the current branch
	gh workflow run spread.yml --ref $(BRANCH)

.PHONY: audit-ci
audit-ci: ## Trigger the Audit workflow on GitHub for the current branch
	gh workflow run audit.yml --ref $(BRANCH)

.PHONY: ci-watch
ci-watch: ## Watch the most recent GitHub Actions run on the current branch
	gh run watch $$(gh run list --branch $(BRANCH) --limit 1 --json databaseId --jq '.[0].databaseId')

# ------------------------------------------------------------------------
# clean
# ------------------------------------------------------------------------

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

.PHONY: clean
clean: clean-snaps ## clean-snaps + Rust/Python build and coverage output
	rm -rf client/target
	rm -rf server/htmlcov server/coverage-*.xml
