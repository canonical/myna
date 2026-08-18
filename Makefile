SHELL := /bin/bash
.DEFAULT_GOAL := help

# Model-backed inference snaps: prepare.sh + snapcraft pack is identical for
# all of them (see `snap_template` below). Model-fetch is not, so it's kept
# as explicit per-snap targets further down.
MODEL_SNAP_DIRS := whisper-snap parakeet-snap nemotron-snap qwen-snap sherpa-snap funasr-snap audio8-snap
# Everything else that also builds via prepare.sh + snapcraft pack.
PLAIN_SNAP_DIRS := myna-snap fake-snap
SNAP_DIRS       := $(MODEL_SNAP_DIRS) $(PLAIN_SNAP_DIRS)

BRANCH := $(shell git branch --show-current)

# ------------------------------------------------------------------------
# help
# ------------------------------------------------------------------------

.PHONY: help
help: ## List targets with descriptions
	@grep -E '^[a-zA-Z0-9_.%-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Per-snap targets (generated, one per */dev/prepare.sh + snapcraft pack):"
	@for d in $(SNAP_DIRS); do printf "  \033[36m%-22s\033[0m %s\n" "$$d" "Stage + snapcraft pack $$d"; done
	@echo
	@echo "  Fetch models + build in one step:"
	@for d in $(MODEL_SNAP_DIRS); do printf "  \033[36m%-22s\033[0m %s\n" "$$d-all" "$$d-models + $$d"; done

# ------------------------------------------------------------------------
# snaps — prepare + pack (uniform across every snap dir)
# ------------------------------------------------------------------------

define snap_template
.PHONY: $(1)
$(1): ## Stage + snapcraft pack $(1)
	cd $(1) && ./dev/prepare.sh && snapcraft pack
endef
$(foreach dir,$(SNAP_DIRS),$(eval $(call snap_template,$(dir))))

.PHONY: snaps
snaps: $(SNAP_DIRS) ## Rebuild every snap (assumes models already fetched)

# ------------------------------------------------------------------------
# snap model fetch — not uniform, kept explicit
# ------------------------------------------------------------------------

.PHONY: whisper-snap-models
whisper-snap-models: ## Fetch whisper-snap model weights (tiny/base/small)
	cd whisper-snap && ./dev/download-models.sh

.PHONY: parakeet-snap-models
parakeet-snap-models: ## Fetch parakeet-snap model weights
	cd parakeet-snap && ./dev/download-models.sh

.PHONY: nemotron-snap-models
nemotron-snap-models: ## Fetch nemotron-snap model weights
	cd nemotron-snap && ./dev/download-models.sh

.PHONY: qwen-snap-models
qwen-snap-models: ## Fetch qwen-snap model weights (0.6B + 1.7B)
	cd qwen-snap && ./dev/download-models.sh

.PHONY: sherpa-snap-models
sherpa-snap-models: ## Fetch sherpa-snap model weights
	cd sherpa-snap && ./dev/download-models.sh

.PHONY: funasr-snap-models
funasr-snap-models: ## Fetch funasr-snap (SenseVoice) model weights
	uv run ./dev/fetch_funasr_model.py \
		--target ./funasr-snap/components/model-sensevoice-onnx

.PHONY: audio8-snap-models
audio8-snap-models: ## Fetch audio8-snap model weights (CC-BY-NC-4.0, non-commercial)
	uv run ./dev/fetch_audio8_model.py \
		--profile snap --target ./audio8-snap/components/model-audio8-onnx \
		--accept-license "CC-BY-NC-4.0"

define snap_all_template
.PHONY: $(1)-all
$(1)-all: $(1)-models $(1) ## Fetch models + build $(1)
endef
$(foreach dir,$(MODEL_SNAP_DIRS),$(eval $(call snap_all_template,$(dir))))

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

.PHONY: test
test: ## Rust test suite (workshop: test)
	workshop run myna test

.PHONY: py-test
py-test: ## Python test suite (workshop: py-test)
	workshop run myna py-test

.PHONY: lint
lint: ## Rust lints as errors (workshop: lint)
	workshop run myna lint

.PHONY: fmt
fmt: ## Rust format check (workshop: fmt)
	workshop run myna fmt

.PHONY: py-lint
py-lint: ## Python lint + format check (workshop: py-lint)
	workshop run myna py-lint

.PHONY: cov
cov: ## Rust coverage (workshop: cov)
	workshop run myna cov

.PHONY: py-cov
py-cov: ## Python coverage (workshop: py-cov)
	workshop run myna py-cov

# Mirrors the `static` job in .github/workflows/ci.yml exactly (fmt, machete,
# deny, py-lint, py-types, shell-lint, workflow-lint) — deliberately excludes
# `lint`/`test`/`py-test`, which are the separate `workshop` CI job.
.PHONY: check
check: fmt py-lint ## All static gates from CI's `static` job
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
