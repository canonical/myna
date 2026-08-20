SHELL := /bin/bash
.DEFAULT_GOAL := help

# Inference snaps, by short name: `make snap-whisper` fetches that snap's model
# weights, stages its wheels and packs it. Fetch is idempotent (every fetcher
# skips weights already staged under components/), so it is folded into the
# build instead of being a separate target you have to remember.
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

.PHONY: lint-snaps
lint-snaps: ## Validate snap engine/runtime/model manifests with modelctl lint-package
	./dev/lint-packages.sh

# ------------------------------------------------------------------------
# client / server / bench
# ------------------------------------------------------------------------

.PHONY: client
client: ## Build the Rust client workspace (release)
	cd client && cargo build --release

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
	sudo server/.venv/bin/python dev/matrix.py --config dev/matrix.yaml --only $* --keep-results

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
		--socket /var/snap/$*/common/run/ubustt.sock \
		--manifest ../corpus/real/manifest-long.json --label $*/long-form

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

.PHONY: test-extension
test-extension: ## GNOME Shell extension suites, incl. the headless-Shell presentation check (workshop: gjs-test)
	workshop run myna gjs-test

.PHONY: lint-client
lint-client: ## Rust lints as errors (workshop: lint)
	workshop run myna lint

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

.PHONY: clean
clean: clean-snaps ## clean-snaps + Rust/Python build and coverage output
	rm -rf client/target
	rm -rf server/htmlcov server/coverage-*.xml
