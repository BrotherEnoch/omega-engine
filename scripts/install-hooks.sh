#!/usr/bin/env bash
# scripts/install-hooks.sh
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

git config core.hooksPath .githooks
chmod +x .githooks/pre-commit .githooks/pre-push

echo "hooksPath set to .githooks"
echo "Done."