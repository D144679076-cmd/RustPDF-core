#!/usr/bin/env bash
set -euo pipefail

# Load PDF_CORE_LICENSE_SECRET from .env if present and not already set.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/../.env"
if [[ -f "$ENV_FILE" && -z "${PDF_CORE_LICENSE_SECRET:-}" ]]; then
  # shellcheck source=/dev/null
  export $(grep -v '^#' "$ENV_FILE" | xargs)
fi

if [[ -z "${PDF_CORE_LICENSE_SECRET:-}" ]]; then
  echo "error: PDF_CORE_LICENSE_SECRET is not set." >&2
  echo "       Copy pdf-editor-rust-core/.env.example to pdf-editor-rust-core/.env and fill it in." >&2
  exit 1
fi

cd "$SCRIPT_DIR/.."
cargo run -q --features crypto --bin keygen -- "$@"
