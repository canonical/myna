#!/bin/bash -eu
# Stage the Python server source into the snap project (craft-parts local
# sources must live inside the project dir; same pattern as myna-snap).
# server/pyproject.toml references ../README.md and ../LICENSE, so the stage
# tree mirrors the repo layout: stage/server/ + stage/README.md + LICENSE.
cd "$(dirname "$0")/.."
rm -rf stage
mkdir -p stage/server stage/scripts
cp -r ../server/src ../server/pyproject.toml stage/server/
cp ../README.md ../LICENSE stage/
cp scripts/server.sh stage/scripts/
echo "staged server/ into fake-snap/stage"
