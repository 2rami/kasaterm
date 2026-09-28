#!/usr/bin/env bash
set -euo pipefail

SOURCE="$(cd "$(dirname "$0")/.." && pwd)"
PYTHON="${KASATERM_PUBLISHER_PYTHON:-python3}"
cd "$SOURCE"
exec "$PYTHON" -m tools.release.auto "$@"
