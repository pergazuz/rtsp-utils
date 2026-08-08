#!/usr/bin/env bash
#
# macOS and Linux entry point. The real work is in run.mjs, which runs the same
# way on every platform.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if command -v bun >/dev/null 2>&1; then
  exec bun run.mjs "$@"
elif command -v node >/dev/null 2>&1; then
  # Node can drive the launcher, though the UI build still needs Bun.
  exec node run.mjs "$@"
fi

echo "error: bun is not installed." >&2
echo "  Install Bun:  curl -fsSL https://bun.sh/install | bash" >&2
exit 1
