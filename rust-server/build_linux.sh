#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
RUN_CHECKS=""
DIAGNOSTICS=0
ARCHES=(amd64)
while (($#)); do
  case "$1" in
    --tests) RUN_CHECKS=1 ;;
    --no-tests) RUN_CHECKS=0 ;;
    --diagnostics) DIAGNOSTICS=1 ;;
    --arch)
      shift
      case "${1:-}" in
        amd64|arm64|armv7) ARCHES=("$1") ;;
        all) ARCHES=(amd64 arm64 armv7) ;;
        *) echo "Usage: $0 [--tests|--no-tests] [--diagnostics] [--arch amd64|arm64|armv7|all]" >&2; exit 2 ;;
      esac
      ;;
    *) echo "Usage: $0 [--tests|--no-tests] [--diagnostics] [--arch amd64|arm64|armv7|all]" >&2; exit 2 ;;
  esac
  shift
done
# Rust target triple for every supported server arch.
rust_target() {
  case "$1" in
    amd64) echo x86_64-unknown-linux-musl ;;
    arm64) echo aarch64-unknown-linux-musl ;;
    armv7) echo armv7-unknown-linux-musleabihf ;;
  esac
}
# Native execution requires a matching CPU architecture as well as Linux.
can_run_target() {
  case "$1:$2" in
    x86_64-*-linux-*:amd64|aarch64-*-linux-*:arm64|armv7-*-linux-*:armv7) return 0 ;;
    *) return 1 ;;
  esac
}
if [[ -z "$RUN_CHECKS" ]]; then
  if [[ -t 0 ]]; then
    read -rp "Запустить проверки и тесты (или их кросс-компиляцию) перед сборкой? [Y/n]: " REPLY
    case "$REPLY" in
      [nN]|[nN][oO]|[нН]|[нН][eE][тТ]) RUN_CHECKS=0 ;;
      *) RUN_CHECKS=1 ;;
    esac
  else
    RUN_CHECKS=1
  fi
fi
FEATURE_ARGS=()
if [[ "$DIAGNOSTICS" == 1 ]]; then
  FEATURE_ARGS=(--features diagnostics)
fi
command -v cargo >/dev/null
command -v rustup >/dev/null
command -v zig >/dev/null
cargo zigbuild --help >/dev/null
rustup toolchain install 1.97.1 --profile minimal --component rustfmt --component clippy
for ARCH in "${ARCHES[@]}"; do rustup target add "$(rust_target "$ARCH")" --toolchain 1.97.1; done
rustc +1.97.1 --version
zig version
HOST="$(rustc +1.97.1 -vV | sed -n 's/^host: //p')"
if [[ "$RUN_CHECKS" == 1 ]]; then
  cargo +1.97.1 fmt --all -- --check
fi
mkdir -p "$ROOT/dist"
for ARCH in "${ARCHES[@]}"; do
  TARGET="$(rust_target "$ARCH")"
  # cargo-zigbuild сам подставляет zig как cc/линкер для цели; свои обёртки
  # дублируют crt1 и ломают линковку arm64.
  if [[ "$RUN_CHECKS" == 1 ]]; then
    cargo +1.97.1 zigbuild --all-targets --target "$TARGET" "${FEATURE_ARGS[@]}"
    CARGO_TARGET_DIR="$ROOT/build/linux-musl-check" RUSTUP_TOOLCHAIN=1.97.1 cargo-zigbuild clippy --release --target "$TARGET" "${FEATURE_ARGS[@]}" --all-targets -- -D warnings
    if can_run_target "$HOST" "$ARCH"; then
      echo "Running Linux musl tests..."
      CARGO_TARGET_DIR="$ROOT/build/linux-musl-tests" RUSTUP_TOOLCHAIN=1.97.1 cargo-zigbuild test --target "$TARGET" "${FEATURE_ARGS[@]}" --all-targets
    else
      echo "Compiling $TARGET test binaries (they cannot run on this host)..."
      CARGO_TARGET_DIR="$ROOT/build/linux-musl-tests" cargo +1.97.1 zigbuild --target "$TARGET" "${FEATURE_ARGS[@]}" --tests
    fi
  fi
  CARGO_TARGET_DIR="$ROOT/build/linux-musl" cargo +1.97.1 zigbuild --release --target "$TARGET" "${FEATURE_ARGS[@]}"
  cp "$ROOT/build/linux-musl/$TARGET/release/csqtt" "$ROOT/dist/csqtt-linux-$ARCH"
done
ls -lh "$ROOT"/dist/csqtt-linux-*
