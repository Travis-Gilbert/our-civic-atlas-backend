#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly TS_PLUGIN="$REPO_ROOT/apps/graphql-server/node_modules/.bin/protoc-gen-ts_proto"
readonly TS_OUT="$REPO_ROOT/apps/graphql-server/src/generated"
readonly PY_OUT="$REPO_ROOT/python/civic_atlas/generated"
readonly PY_GRPCIO_TOOLS_VERSION="1.80.0"

readonly PROTO_FILES=(
    "proto/civic_atlas/v1/civic_atlas.proto"
    "proto/civic_atlas/v1/reconstruction.proto"
    "proto/civic_atlas/v1/reconstruction_service.proto"
    "proto/civic_atlas/v1/spacetime_atlas.proto"
    "proto/theseus_bridge/v1/bridge.proto"
)

if ! command -v protoc >/dev/null 2>&1; then
    echo "protoc is required to generate Civic Atlas proto artifacts" >&2
    exit 1
fi

if [[ ! -x "$TS_PLUGIN" ]]; then
    echo "ts-proto plugin not found; run npm install in apps/graphql-server" >&2
    exit 1
fi

if ! command -v uvx >/dev/null 2>&1; then
    echo "uvx is required to run grpcio-tools for Python generation" >&2
    exit 1
fi

rm -rf \
    "$TS_OUT/civic_atlas" \
    "$TS_OUT/theseus_bridge" \
    "$PY_OUT/civic_atlas" \
    "$PY_OUT/theseus_bridge"
mkdir -p "$TS_OUT" "$PY_OUT"

(
    cd "$REPO_ROOT"
    protoc \
        -I proto \
        "--plugin=protoc-gen-ts_proto=$TS_PLUGIN" \
        "--ts_proto_out=$TS_OUT" \
        --ts_proto_opt=esModuleInterop=true,importSuffix=.js,outputServices=generic-definitions,useExactTypes=false \
        "${PROTO_FILES[@]}"

    uvx --from "grpcio-tools==$PY_GRPCIO_TOOLS_VERSION" python -m grpc_tools.protoc \
        -I proto \
        "--python_out=$PY_OUT" \
        "--grpc_python_out=$PY_OUT" \
        "${PROTO_FILES[@]}"
)
