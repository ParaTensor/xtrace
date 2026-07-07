#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -z "${DATABASE_URL:-}" ]]; then
  echo "DATABASE_URL is required" >&2
  exit 1
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${1:-./backups}"
mkdir -p "$OUT_DIR"

FILE="$OUT_DIR/xtrace-${STAMP}.sql"
pg_dump "$DATABASE_URL" > "$FILE"
echo "backup written to $FILE"
