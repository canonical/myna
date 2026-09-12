#!/bin/bash
# Strip payloads pip installs that no runtime path in this snap executes.
# $1 is the site-packages dir, $2 the python to gate with.
#
# PRUNE_DROP  extra top-level distributions to drop, snap-specific
# PRUNE_GATE  python run once the prune is done; a non-zero exit fails the build
#
# Dropping a declared dependency holds only while nothing on the runtime path
# reaches it, which is upstream's choice to change, not ours. PRUNE_GATE is
# where each snap re-checks its own version of that on every build, so a
# regression surfaces here instead of on a user's first transcribe.
set -euo pipefail

# `:?` rather than a bare assignment: every rm below builds a path from $site,
# and an empty one would aim them at /.
site="${1:?site directory required}"
python="${2:?python interpreter required}"

drop() {
	local p
	for p in "$@"; do
		rm -rf "${site:?}/$p" "${site:?}/${p}-"*.dist-info
	done
}

# The build-time resolver, and huggingface_hub's transfer backend - every
# hf_xet import site is function-local in an upload/download path, and no
# inference snap here holds the `network` plug.
drop pip hf_xet

# numpy's test suite. The public numpy.testing helpers live elsewhere.
rm -rf "$site"/numpy/tests "$site"/numpy/*/tests "$site"/numpy/*/*/tests

# onnxruntime's model-conversion and quantisation tooling. __init__ reaches
# .transformers only from a debug dump gated on cuda_version plus cpuinfo plus
# py3nvml, and none of the three is installed.
rm -rf "$site"/onnxruntime/transformers "$site"/onnxruntime/quantization \
	"$site"/onnxruntime/tools "$site"/onnxruntime/datasets \
	"$site"/onnxruntime/backend

# PRUNE_DROP is a space-separated list from the calling snapcraft part, so it
# has to split - through an array, not by leaving an expansion unquoted.
read -ra extra_drops <<<"${PRUNE_DROP:-}"
drop "${extra_drops[@]}"

PYTHONPATH="$site${PYTHONPATH:+:$PYTHONPATH}" "$python" -c "${PRUNE_GATE:?gate required}"
echo "prune-runtime: gate passed"
