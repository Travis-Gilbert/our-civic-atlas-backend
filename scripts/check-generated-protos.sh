#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly PY_GRPCIO_TOOLS_VERSION="1.80.0"
readonly PY_GENERATED_ROOT="$REPO_ROOT/python/civic_atlas/generated"

"$SCRIPT_DIR/generate-protos.sh"

(
    cd "$REPO_ROOT"
    git diff --exit-code \
        -I '^//   protoc[[:space:]]*v' \
        -- \
        apps/graphql-server/src/generated \
        python/civic_atlas/generated

    untracked=$(git ls-files --others --exclude-standard -- \
        apps/graphql-server/src/generated \
        python/civic_atlas/generated)
    if [[ -n "$untracked" ]]; then
        echo "Generated proto artifacts are not tracked:" >&2
        echo "$untracked" >&2
        exit 1
    fi

    PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PY_GENERATED_ROOT" \
        uvx --from "grpcio-tools==$PY_GRPCIO_TOOLS_VERSION" \
        python "$REPO_ROOT/scripts/check_generated_python_imports.py"
)
