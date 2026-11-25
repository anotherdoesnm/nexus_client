#!/usr/bin/env bash
# Build static musl binaries for amd64 and arm64.
#
# Requirements:
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#   cargo install cargo-zigbuild
#   zig 0.13+ on PATH (https://ziglang.org/download/) — it supplies the C
#   toolchain for ring and the musl libc, so no cross-gcc is needed.
#
# The result lands in dist/ as nexus-tui-client-<version>-<target>.tar.gz,
# each a single statically linked binary (no glibc, no shared libraries).
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGETS=("$@")
if [ ${#TARGETS[@]} -eq 0 ]; then
  TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

command -v zig >/dev/null || { echo "zig is not on PATH" >&2; exit 1; }
cargo zigbuild --version >/dev/null || { echo "cargo-zigbuild is not installed" >&2; exit 1; }

mkdir -p dist
for target in "${TARGETS[@]}"; do
  echo "==> $target"
  CARGO_INCREMENTAL=0 cargo zigbuild \
    --release \
    --no-default-features \
    --target "$target" \
    --bin nexus-tui-client

  binary="target/$target/release/nexus-tui-client"
  file "$binary"
  file "$binary" | grep -q 'statically linked' || {
    echo "$binary is not statically linked" >&2
    exit 1
  }

  name="nexus-tui-client-${VERSION}-${target}"
  stage="dist/${name}"
  rm -rf "$stage"
  mkdir -p "$stage"
  cp "$binary" "$stage/"
  cp README.md "$stage/"
  cp docs/MIGRATION.md docs/protocol-bot.md docs/protocol-web.md "$stage/"
  tar -C dist -czf "dist/${name}.tar.gz" "$name"
  ( cd dist && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256" )
done

ls -l dist
