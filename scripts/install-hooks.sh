#!/usr/bin/env bash
# Install foch git hooks for the current worktree's .git/hooks dir.
# Idempotent: safe to re-run.
set -e
cd "$(git rev-parse --show-toplevel)"
HOOK_DIR="$(git rev-parse --git-path hooks)"
mkdir -p "$HOOK_DIR"
for hook in pre-commit pre-push; do
  src="$(pwd)/scripts/hooks/$hook"
  dst="$HOOK_DIR/$hook"
  if [[ -e "$dst" && ! -L "$dst" ]]; then
    echo "[foch] preserving existing $dst → $dst.bak"
    mv "$dst" "$dst.bak"
  fi
  ln -sf "$src" "$dst"
  chmod +x "$src"
  echo "[foch] installed: $dst → $src"
done
echo "[foch] hooks installed. Override individually with FOCH_SKIP_PRE_COMMIT=1 / FOCH_SKIP_PRE_PUSH=1."
