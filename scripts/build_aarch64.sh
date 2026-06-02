#!/usr/bin/env bash
# Cross-compile a statically linked aarch64-unknown-linux-musl xtrace binary
# (Debian/Ubuntu host). Set INSTALL_BUILD_DEPS=1 to run apt install via sudo.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT}"

OUTPUT_BINARY="${OUTPUT_BINARY:-${ROOT}/xtrace-aarch64}"
ARTIFACT="${ARTIFACT:-${ROOT}/xtrace.tar.gz}"
CARGO_TARGET="aarch64-unknown-linux-musl"
BUILD_PROFILE="${BUILD_PROFILE:-release}"

if [ "${INSTALL_BUILD_DEPS:-0}" = "1" ]; then
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "INSTALL_BUILD_DEPS=1 requires apt-get (Debian/Ubuntu)." >&2
    exit 1
  fi
  echo "==> Installing build dependencies (sudo)..."
  sudo apt-get update
  sudo apt-get install -y gcc-aarch64-linux-gnu musl-tools
fi

if ! command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
  echo "Missing aarch64-linux-gnu-gcc. On Debian/Ubuntu run:" >&2
  echo "  sudo apt-get install -y gcc-aarch64-linux-gnu musl-tools" >&2
  echo "Or re-run with INSTALL_BUILD_DEPS=1" >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup is required." >&2
  exit 1
fi

echo "==> Adding Rust target ${CARGO_TARGET}..."
rustup target add "${CARGO_TARGET}"

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc

echo "==> Building xtrace (${BUILD_PROFILE}, ${CARGO_TARGET})..."
if [ "${BUILD_PROFILE}" = "release" ]; then
  cargo build --release --target "${CARGO_TARGET}"
else
  cargo build --target "${CARGO_TARGET}"
fi

built="target/${CARGO_TARGET}/${BUILD_PROFILE}/xtrace"
if [ ! -f "${built}" ]; then
  echo "Binary not found: ${built}" >&2
  exit 1
fi

cp "${built}" "${OUTPUT_BINARY}"
chmod +x "${OUTPUT_BINARY}"

echo "==> Packaging ${ARTIFACT}..."
tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT
cp "${OUTPUT_BINARY}" "${tmpdir}/xtrace"
tar -czf "${ARTIFACT}" -C "${tmpdir}" xtrace

echo "==> Done."
ls -lh "${OUTPUT_BINARY}" "${ARTIFACT}"
