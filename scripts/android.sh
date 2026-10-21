#!/bin/sh
set -eu
# Per-command defaults; do not change the user's shell or global Java selection.
cd "$(dirname "$0")/.."
exec node scripts/android.mjs "$@"
