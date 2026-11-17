#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${TARGET:-x86_64-unknown-linux-musl}"
BIN_NAME="comfy-bot"

if ! command -v zig >/dev/null 2>&1; then
    echo "zig is required. Install it with: brew install zig" >&2
    exit 1
fi

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "cargo-zigbuild is required. Install it with: cargo install cargo-zigbuild" >&2
    exit 1
fi

rustup target add "$TARGET"

cargo zigbuild --release --target "$TARGET"

mkdir -p dist
OUT="dist/${BIN_NAME}-${TARGET}"
cp "target/${TARGET}/release/${BIN_NAME}" "$OUT"

file "$OUT"
echo "Built $OUT"
