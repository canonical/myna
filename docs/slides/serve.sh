#!/usr/bin/env bash
# Serve the slide decks in this directory on a local HTTP server.
# Usage: ./serve.sh [port]   (default port: 8000)
set -euo pipefail

PORT="${1:-8000}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Serving ${DIR} at http://localhost:${PORT}/audio-adapter-overview.html"
echo "Press Ctrl+C to stop."
exec python3 -m http.server "${PORT}" --bind 127.0.0.1 --directory "${DIR}"
