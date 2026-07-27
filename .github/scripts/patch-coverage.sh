#!/usr/bin/env bash
# Fail unless every line this pull request changed is covered by a test —
# ADR-0010's headline gate, 100% patch coverage.
#
# A script rather than an inline `run:` block for two reasons: shellcheck can
# read it (actionlint only reaches `.github/workflows`, so shell embedded in a
# composite action is the one shell nothing checks), and the workflow can pass
# every input through the environment instead of interpolating `${{ }}` into it.
#
# Usage: patch-coverage.sh <lcov-report> <base-branch> [source-path-prefix]
set -euo pipefail

report="${1:?usage: patch-coverage.sh <lcov-report> <base-branch> [path-prefix]}"
base="${2:?usage: patch-coverage.sh <lcov-report> <base-branch> [path-prefix]}"
prefix="${3:-}"

# The base only exists locally if it was fetched; actions/checkout brings down
# the merge ref, not the branch it targets.
git fetch --no-tags origin "+refs/heads/${base}:refs/remotes/origin/${base}"

# diff-cover matches the report's paths against git's, which are relative to the
# repository root. A report written from a subdirectory (Vitest, under Vite's
# `client/` root) needs that subdirectory put back on the front.
if [ -n "$prefix" ]; then
  rewritten="$(mktemp)"
  sed "s|^SF:|SF:${prefix}|" "$report" >"$rewritten"
  report="$rewritten"
fi

# `pipx run` installs into a throwaway environment and executes in one step, so
# nothing depends on where pipx puts its shims on PATH.
pipx run diff-cover "$report" \
  --compare-branch "origin/${base}" \
  --show-uncovered \
  --fail-under 100
