#!/bin/sh
# Install local-fork git hooks and remote guards for this purely-local fork of
# zed (lherron/zed). Idempotent — safe to re-run. Run after a fresh clone:
#
#   ./scripts/install-local-fork-hooks.sh
#
# This fork is local-only and must NEVER be pushed/PR'd to zed-industries/zed.

set -e

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

# 1. Install the pre-push guard hook.
hooks_dir=$(git rev-parse --git-path hooks)
mkdir -p "$hooks_dir"
cp scripts/git-hooks/pre-push "$hooks_dir/pre-push"
chmod +x "$hooks_dir/pre-push"
echo "installed: $hooks_dir/pre-push"

# 2. Disable pushing to the upstream remote (if present).
if git remote get-url upstream >/dev/null 2>&1; then
  git remote set-url --push upstream DISABLED_NO_UPSTREAM_PUSH
  echo "disabled push URL on remote 'upstream'"
fi

# 3. Make bare 'git push' always target the fork.
if git remote get-url origin >/dev/null 2>&1; then
  git config remote.pushDefault origin
  echo "set remote.pushDefault = origin"
fi

echo "done — upstream pushes are now blocked at multiple layers."
