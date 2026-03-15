#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/website"
TARGET_DIR="${1:-/var/www/memoryoss.com}"

if [[ ! -d "$SRC_DIR" ]]; then
  echo "missing website source directory: $SRC_DIR" >&2
  exit 1
fi

install -d -m 755 "$TARGET_DIR"

rsync -a --delete --delete-excluded \
  --exclude='*.bak' \
  --exclude='*.bak-*' \
  "$SRC_DIR"/ "$TARGET_DIR"/

find "$TARGET_DIR" -type d -exec chmod 755 {} +
find "$TARGET_DIR" -type f -exec chmod 644 {} +
find "$TARGET_DIR" -type f -name '*.sh' -exec chmod 755 {} +

echo "Deployed website from $SRC_DIR to $TARGET_DIR"
