#!/usr/bin/env bash
# What the nightly mutation sweep sweeps, and whether it finished (#121).
#
# The decisions about what to sweep live here, each a subcommand that is a
# function of its arguments and its input — so tests/nightly.rs runs every one,
# which a workflow cannot be until it is pushed. `plan` is the one that is
# not: it is the adapter, gathering the inputs (run history, git,
# cargo-mutants' count) and handing them to the decisions. (What is *filed*
# is nightly-report.sh's.)
#
#   dispatch-mode FIRED FULL_CRON WEEKDAY   what a schedule tick dispatches
#   baselines < RUNS                        the last green sweep, and full sweep
#   decide REQUESTED HEAD GREEN FULL CHOSEN KNOWN
#                                           how much a dispatched sweep mutates
#   size COUNT                              how many shards, and their matrix
#   tickets < SUBJECTS                      the #tickets a range of commits names
#   completed STATUS                        whether cargo-mutants finished
#   plan                                    all of it, for the plan job
#
# Written for bash 3.2, because tests/nightly.rs runs it on a Mac too.
set -euo pipefail

# Mutants a shard is given. At roughly a minute and a half each (two at a time,
# each a rebuild plus the whole suite), 85 is about two hours: well inside the
# sweep job's 210-minute limit, with room for the suite to keep growing. The
# number to watch is a full sweep's per-shard wall clock; when it nears three
# hours, lower this.
PER_SHARD=85

# GitHub's ceiling on the jobs one matrix may expand to.
MAX_SHARDS=256

# What a schedule tick dispatches: `full` on the weekly tick, `skip` on the
# daily tick of the day the full sweep runs (it is everything an incremental
# sweep would be, and more), and `incremental` otherwise. The day is read off
# the full sweep's own cron, so the two cannot be moved apart.
#
# WEEKDAY is `date -u +%u`: 1 is Monday and 7 is Sunday, where cron's own
# Sunday is 0 or 7.
dispatch_mode() {
  local fired=$1 full_cron=$2 weekday=$3 day
  if [ "$fired" = "$full_cron" ]; then
    echo full
    return
  fi
  day=$(printf '%s\n' "$full_cron" | awk '{ print $5 }')
  [ "$day" = 0 ] && day=7
  if [ "$weekday" = "$day" ]; then
    echo skip
  else
    echo incremental
  fi
}

# Where a sweep can start. Reads the green sweep runs of one branch, newest
# first, as `head-sha<TAB>run name`, and prints `<last green> <last full>`, with
# `-` for none. A run named `... from <sha>` swept a hand-picked range, which
# says nothing about the commits before it, so it is neither.
baselines() {
  awk -F '\t' '
    $2 ~ / from / { next }
    green == "" { green = $1 }
    full == "" && $2 ~ /\(full\)/ { full = $1 }
    END {
      print (green == "" ? "-" : green), (full == "" ? "-" : full)
    }'
}

# How much a dispatched sweep mutates: prints `<mode> <baseline> <why>`, where
# mode is `incremental`, `full` or `skip` and baseline is `-` unless
# incremental.
#
#   REQUESTED  what was dispatched: `incremental` or `full`
#   HEAD       the commit being swept
#   GREEN      the last green sweep's commit, or `-`
#   FULL       the last green full sweep's commit, or `-`
#   CHOSEN     a hand-picked baseline, or `-`
#   KNOWN      whether this checkout has the commit an incremental sweep would
#              start from (CHOSEN if given, else GREEN): `yes` or `no`
#
# A sweep that falls back to full keeps the name it was dispatched under —
# `(incremental)` — because a run's name is fixed before this decides, so
# `baselines` will not count it as a full sweep and the next weekly one runs
# even if nothing moved. It happens only on a branch's first sweep and after
# history is rewritten, and costs at most one redundant full sweep; seeding a
# new branch with a dispatched full sweep avoids it entirely.
decide() {
  local requested=$1 head=$2 green=$3 full=$4 chosen=$5 known=$6
  if [ "$requested" = full ]; then
    if [ "$full" = "$head" ]; then
      echo "skip - nothing has landed since the last full sweep"
    else
      echo "full - the weekly full sweep"
    fi
    return
  fi
  if [ "$chosen" != - ]; then
    if [ "$known" != yes ]; then
      echo "the baseline $chosen is not a commit on this branch" >&2
      return 2
    fi
    echo "incremental $chosen from a hand-picked baseline"
    return
  fi
  if [ "$green" = - ]; then
    echo "full - the first sweep of this branch"
  elif [ "$known" != yes ]; then
    # History was rewritten under the last sweep: there is no diff to take.
    echo "full - the last swept commit is no longer in history"
  elif [ "$green" = "$head" ]; then
    echo "skip - nothing has landed since the last sweep"
  else
    echo "incremental $green what landed since the last sweep"
  fi
}

# How COUNT mutants are sharded: prints `<total> <matrix>`, the matrix being
# `[0,1,…]` as JSON, PER_SHARD to a shard. One answer for both, because the
# shards are told `--shard k/<total>`, and a total the matrix disagreed with
# would hand some mutants to no shard at all. Nothing to mutate is `0 []`.
size() {
  local count=$1 total index json
  total=$(((count + PER_SHARD - 1) / PER_SHARD))
  [ "$total" -gt "$MAX_SHARDS" ] && total=$MAX_SHARDS
  json='['
  index=0
  while [ "$index" -lt "$total" ]; do
    [ "$index" -gt 0 ] && json="$json,"
    json="$json$index"
    index=$((index + 1))
  done
  echo "$total $json]"
}

# The tickets a range of commits names: every `#N` in the subjects on stdin,
# once each, in numeric order, space-separated.
tickets() {
  { grep -oE '#[0-9]+' || true; } | sort -t '#' -k 2 -n -u | tr '\n' ' ' | sed 's/ $//'
  echo
}

# Whether cargo-mutants' exit STATUS means it tested everything it was given:
# 0 (all caught), 2 (survivors, which are filed) and 3 (mutants that hung the
# suite until they were stopped — a catch, not a survival, kept in the run's
# artifacts and not filed) are a finished sweep. Anything else — 1 usage, 4 a
# baseline suite already red, 5 and 6 a diff that does not fit the tree, 70
# its own crash — is a sweep that did not happen.
completed() {
  case $1 in
    0 | 2 | 3) return 0 ;;
    *)
      echo "::error::cargo-mutants exited $1: the sweep did not complete"
      return 1
      ;;
  esac
}

# The plan job: gather the inputs, decide, and write the decision to
# $GITHUB_OUTPUT for the shards and the report.
#
# Reads MODE and BASELINE (the dispatch's inputs), BRANCH, HEAD_SHA and
# GH_TOKEN from the environment. The mutants are counted by listing them, which reads the source and
# compiles nothing.
plan() {
  local requested=${MODE:?MODE is what was dispatched: incremental or full}
  local branch=${BRANCH:?BRANCH is the branch being swept}
  local head=${HEAD_SHA:?HEAD_SHA is the commit being swept}
  local runs green full chosen candidate known decided mode baseline why
  local count range tickets total matrix scratch
  scratch=$(mktemp -d)
  chosen=${BASELINE:-}
  [ -n "$chosen" ] || chosen=-

  runs=$(gh run list --workflow nightly.yml --branch "$branch" \
    --event workflow_dispatch --status success --limit 100 \
    --json headSha,displayTitle --jq '.[] | "\(.headSha)\t\(.displayTitle)"')
  read -r green full <<< "$(printf '%s\n' "$runs" | sed '/^$/d' | baselines)"

  candidate=$green
  [ "$chosen" != - ] && candidate=$chosen
  known=no
  if [ "$candidate" != - ] && git cat-file -e "$candidate^{commit}" 2> /dev/null; then
    known=yes
  fi

  decided=$(decide "$requested" "$head" "$green" "$full" "$chosen" "$known")
  read -r mode baseline why <<< "$decided"

  count=0
  # Listed to a file, never piped into the count: under `pipefail` a failing
  # listing would be swallowed by the `|| true` that an empty count needs, and
  # a sweep that could not list its mutants would skip the night *green*.
  case $mode in
    incremental)
      git diff "$baseline...$head" > "$scratch/sweep.diff"
      cargo mutants --list --in-diff "$scratch/sweep.diff" > "$scratch/mutants.txt"
      count=$(grep -c '\.rs:' "$scratch/mutants.txt" || true)
      range="${baseline:0:7}..${head:0:7}"
      tickets=$(git log --format=%s "$baseline..$head" | tickets)
      ;;
    full)
      cargo mutants --list > "$scratch/mutants.txt"
      count=$(grep -c '\.rs:' "$scratch/mutants.txt" || true)
      range="at ${head:0:7}"
      tickets=''
      ;;
    *)
      range="at ${head:0:7}"
      tickets=''
      ;;
  esac
  read -r total matrix <<< "$(size "$count")"
  if [ "$mode" != skip ] && [ "$total" -eq 0 ]; then
    mode=skip
    why="nothing in it can be mutated"
  fi
  # A skipped sweep still names one shard: GitHub can refuse an empty matrix
  # before it reads the `if:` that skips the job, and that shard never runs.
  [ "$mode" = skip ] && matrix='[0]'

  echo "::notice::$mode sweep of $branch $range: $why ($count mutants, $total shards)"
  {
    echo "mode=$mode"
    echo "baseline=$baseline"
    echo "total=$total"
    echo "shards=$matrix"
    echo "range=$range"
    echo "tickets=$tickets"
  } >> "${GITHUB_OUTPUT:?the plan is written for the jobs after it}"
}

command=${1:-}
[ $# -gt 0 ] && shift
case $command in
  dispatch-mode) dispatch_mode "$@" ;;
  baselines) baselines ;;
  decide) decide "$@" ;;
  size) size "$@" ;;
  tickets) tickets ;;
  completed) completed "$@" ;;
  plan) plan ;;
  *)
    echo "usage: $0 dispatch-mode|baselines|decide|size|tickets|completed|plan" >&2
    exit 64
    ;;
esac
