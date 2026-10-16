#!/usr/bin/env bash
# A stand-in for the `gh` CLI, for tests/nightly.rs (#121).
#
# Just enough of `gh issue` to keep issues in a directory ($FAKE_GH_STATE), so a
# test asserts on what the nightly's scripts *left behind* — which issues are
# open, what their bodies list, what was said on them — rather than on which
# calls were made in which order. A verb it was not written for fails, and so
# does one missing the flag the scripts depend on (`list` must ask for open
# issues, `view` for the body); output shaping (`--json`, `--jq`, `--limit`) is
# answered in the one shape the scripts ask for, rather than interpreted.
#
# FAKE_GH_FAIL names a verb that fails instead, for a test of what happens when
# GitHub refuses one.
#
# Written for bash 3.2, because that is the bash a Mac runs the suite with.
set -euo pipefail

state=${FAKE_GH_STATE:?FAKE_GH_STATE names the directory issues are kept in}
issues="$state/issues"
mkdir -p "$issues"

unsupported() {
  echo "fake gh: unsupported: $*" >&2
  exit 64
}

[ "${1:-}" = issue ] || unsupported "$@"
verb=${2:-}
shift 2

if [ "$verb" = "${FAKE_GH_FAIL:-}" ]; then
  echo "fake gh: refusing to $verb, as asked" >&2
  exit 1
fi

case $verb in
  list)
    case " $* " in *" --state open "*) ;; *) unsupported list "$@" ;; esac
    # The scripts filter by title themselves, so the fake answers every open
    # issue in the shape their `--jq` asks for: number, a tab, the title.
    for dir in "$issues"/*/; do
      [ -d "$dir" ] || continue
      [ "$(cat "$dir/state")" = open ] || continue
      printf '%s\t%s\n' "$(basename "$dir")" "$(cat "$dir/title")"
    done
    ;;
  view)
    case " $* " in *" --json body "*) ;; *) unsupported view "$@" ;; esac
    cat "$issues/$1/body"
    printf '\n'
    ;;
  create)
    title='' body='' labels=''
    while [ $# -gt 0 ]; do
      case $1 in
        --title) title=$2 ;;
        --body-file) body=$2 ;;
        --label) labels="$labels$2"$'\n' ;;
        *) unsupported create "$@" ;;
      esac
      shift 2
    done
    number=$(($(find "$issues" -mindepth 1 -maxdepth 1 -type d | wc -l) + 1))
    dir="$issues/$number"
    mkdir "$dir"
    printf '%s' "$title" > "$dir/title"
    cp "$body" "$dir/body"
    printf '%s' "$labels" > "$dir/labels"
    echo open > "$dir/state"
    : > "$dir/comments"
    echo "https://github.com/fake/repo/issues/$number"
    ;;
  edit)
    [ "${2:-}" = --body-file ] || unsupported edit "$@"
    cp "$3" "$issues/$1/body"
    ;;
  comment)
    [ "${2:-}" = --body-file ] || unsupported comment "$@"
    { cat "$3"; printf '\n\x1e\n'; } >> "$issues/$1/comments"
    ;;
  close)
    dir="$issues/$1"
    echo closed > "$dir/state"
    if [ "${2:-}" = --comment ]; then
      printf '%s\n\x1e\n' "$3" >> "$dir/comments"
    elif [ $# -gt 1 ]; then
      unsupported close "$@"
    fi
    ;;
  *)
    unsupported "$verb" "$@"
    ;;
esac
