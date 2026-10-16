#!/usr/bin/env bash
# File what a nightly mutation sweep found, as two rolling issues (#121).
#
#   sweep PLANNED SWEPT TOTAL DIR   the report job, whole: given the plan's and
#                                   the shards' results and the shard count,
#                                   file breakage, or file the survivors in
#                                   DIR/mutants-shard-<k>/missed.txt and call
#                                   any breakage over. Exits 1 when broken.
#   survivors FILE                  file the survivors listed in FILE
#   broken DETAILS                  file that the sweep did not complete
#   recovered                       a sweep completed; close any breakage
#
# The context is the environment the workflow already sets: BRANCH and RUN
# always, MODE (incremental or full), RANGE and TICKETS for survivors, and
# FAILED_JOBS for a broken sweep's details.
#
# Two issues rather than one, because they want different people doing
# different things: a survivor wants a test written (or a proof, if it is
# equivalent), and a broken sweep wants CI fixed. Both are labelled for
# triage — never for an agent — because which survivors are worth a ticket is
# a judgement, and #83 found a "known equivalent" one that was not.
#
# Each issue is found by its exact title among the open ones, so closing it is
# all it takes to start a fresh one. The survivors list in its body belongs to
# this script and is rewritten whole; the comments are where people talk.
#
# Written for bash 3.2, because tests/nightly.rs runs it on a Mac too.
#
# Every backtick in a single-quoted string here is Markdown — a code span in an
# issue — and expanding one would be the bug, so SC2016 is off for the file.
# shellcheck disable=SC2016
set -euo pipefail

branch=${BRANCH:?BRANCH is the branch that was swept}
run=${RUN:?RUN is the link to the run that is reporting}

START='<!-- survivors: start -->'
END='<!-- survivors: end -->'

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The number of the open issue titled exactly TITLE, or nothing. awk reads to
# the end rather than stopping at the first match: under `pipefail`, a reader
# that exits early can cost `gh` a SIGPIPE, and the script with it.
open_issue() {
  gh issue list --state open --limit 200 --json number,title \
    --jq '.[] | "\(.number)\t\(.title)"' \
    | awk -F '\t' -v title="$1" '$2 == title && found == "" { found = $1 } END { if (found != "") print found }'
}

# Open an issue for triage, printing its number.
open_new() {
  local url
  url=$(gh issue create --title "$1" --label needs-triage --label area:testing \
    --body-file "$2")
  echo "${url##*/}"
}

# The survivors an issue body lists, one per line.
listed() {
  awk -v start="$START" -v end="$END" '
    $0 == end { inside = 0 }
    inside && /^- `/ { sub(/^- `/, ""); sub(/`$/, ""); print }
    $0 == start { inside = 1 }'
}

# Reconcile what was found with what is listed: prints `new <survivor>` for
# each found survivor nothing listed accounts for, then `gone <n>`, the listed
# survivors nothing found accounts for.
#
# A survivor's line is where it was when it was found, and the code around it
# moves every ticket — so one that moved must not come back as new. But two
# distinct mutants can differ *only* in position (two `+` in one function), so
# position cannot simply be dropped either. Each listed survivor accounts for
# one found one: the same position first, then the same mutant anywhere.
reconcile() {
  awk -v listed="$1" '
    function unplaced(line) { sub(/:[0-9]+:[0-9]+: /, ": ", line); return line }
    BEGIN {
      while ((getline line < listed) > 0) {
        if (line == "") continue
        exact[line]++
        moved[unplaced(line)]++
        total++
      }
    }
    $0 != "" { found[++n] = $0 }
    END {
      for (i = 1; i <= n; i++) {
        if (exact[found[i]] > 0) {
          exact[found[i]]--; moved[unplaced(found[i])]--; known[i] = 1; matched++
        }
      }
      for (i = 1; i <= n; i++) {
        if (!known[i] && moved[unplaced(found[i])] > 0) {
          moved[unplaced(found[i])]--; known[i] = 1; matched++
        }
      }
      for (i = 1; i <= n; i++) if (!known[i]) print "new " found[i]
      print "gone " (total - matched)
    }' "$2"
}

# Lines as the body's list.
as_list() {
  local line
  while IFS= read -r line; do
    [ -n "$line" ] && printf -- '- `%s`\n' "$line"
  done
  return 0
}

count() {
  wc -l < "$1" | tr -d ' '
}

# The survivors issue's body, around the list in FILE.
survivors_body() {
  cat << EOF
Mutants the nightly sweep of \`$branch\` found that no test kills — each one a change to the code that the whole suite let through. Filed by the nightly ([#121](../issues/121)); labelled for triage, because which of these is worth a ticket, and which is an equivalent mutant that wants its proof in \`.cargo/mutants.toml\` instead, is a judgement.

An incremental sweep adds what it finds here; the weekly full sweep rewrites the list, and closes this issue when it finds none. The list belongs to the nightly and is rewritten whole, so talk in the comments. Lines are where each mutant was when it was found.

$START
$(as_list < "$1")
$END
EOF
}

survivors() {
  local mode=${MODE:?MODE is incremental or full}
  local range=${RANGE:-} tickets=${TICKETS:-}
  local title number new gone
  title="Mutation survivors on $branch"
  number=$(open_issue "$title")

  sed '/^$/d' "$1" | sort -u > "$work/found"
  : > "$work/listed"
  if [ -n "$number" ]; then
    gh issue view "$number" --json body --jq .body | listed > "$work/listed"
  fi
  reconcile "$work/listed" "$work/found" > "$work/reconciled"
  sed -n 's/^new //p' "$work/reconciled" > "$work/new"
  new=$(count "$work/new")
  gone=$(sed -n 's/^gone //p' "$work/reconciled")

  case $mode in
    incremental)
      # An incremental sweep saw only what changed, so it can only add.
      [ "$new" -gt 0 ] || return 0
      cat "$work/listed" "$work/new" > "$work/list"
      survivors_body "$work/list" > "$work/body"
      if [ -n "$number" ]; then
        gh issue edit "$number" --body-file "$work/body"
      else
        number=$(open_new "$title" "$work/body")
      fi
      {
        printf '**%s new** in the incremental sweep of `%s` %s%s: %s\n\n' \
          "$new" "$branch" "$range" "${tickets:+ ($tickets)}" "$run"
        as_list < "$work/new"
      } > "$work/comment"
      gh issue comment "$number" --body-file "$work/comment"
      ;;
    full)
      # A full sweep saw everything: what it did not find is not surviving.
      if [ ! -s "$work/found" ]; then
        if [ -n "$number" ]; then
          gh issue close "$number" \
            --comment "The full sweep of \`$branch\` $range found none: $run"
        fi
        return 0
      fi
      survivors_body "$work/found" > "$work/body"
      if [ -n "$number" ]; then
        gh issue edit "$number" --body-file "$work/body"
      else
        number=$(open_new "$title" "$work/body")
      fi
      printf 'The full sweep of `%s` %s: **%s survivors**, %s new and %s no longer surviving: %s\n' \
        "$branch" "$range" "$(count "$work/found")" "$new" "$gone" "$run" > "$work/comment"
      gh issue comment "$number" --body-file "$work/comment"
      ;;
    *)
      echo "survivors: MODE is incremental or full, not $mode" >&2
      return 64
      ;;
  esac
}

broken() {
  local details=$1 title number
  title="Mutation sweep broken on $branch"
  number=$(open_issue "$title")
  if [ -n "$number" ]; then
    printf 'Still broken: %s\n\n%s\n' "$run" "$details" > "$work/comment"
    gh issue comment "$number" --body-file "$work/comment"
    return
  fi
  cat > "$work/body" << EOF
The nightly mutation sweep of \`$branch\` did not complete, so it is reporting nothing: no survivors are being found until it does. This is the sweep failing to run, not the survivors it found ("Mutation survivors on $branch" is those). Filed by the nightly ([#121](../issues/121)), commented on each night it stays broken, and closed by the next sweep that completes.

First broken: $run

$details
EOF
  open_new "$title" "$work/body" > /dev/null
}

recovered() {
  local number
  number=$(open_issue "Mutation sweep broken on $branch")
  [ -n "$number" ] || return 0
  gh issue close "$number" --comment "The sweep of \`$branch\` completed again: $run"
}

# The report job: everything above, in the one order that cannot lose a
# failure — survivors before the breakage is called over, so a filing that
# fails is itself filed rather than landing just after the breakage issue
# closed.
sweep() {
  local planned=$1 swept=$2 total=$3 dir=$4 shard
  if [ "$planned" != success ] || [ "$swept" != success ]; then
    broken "${FAILED_JOBS:-The plan: $planned. The shards: $swept.}"
    return 1
  fi

  # Every shard's results, or the sweep did not complete: a shard read as
  # "found nothing" because its results never arrived would let a full sweep
  # close the survivors issue on part of the evidence.
  : > "$work/swept"
  shard=0
  while [ "$shard" -lt "$total" ]; do
    if [ ! -f "$dir/mutants-shard-$shard/missed.txt" ]; then
      broken "Every job succeeded, but shard $shard of $total left no results, so what this sweep found is unknown."
      return 1
    fi
    cat "$dir/mutants-shard-$shard/missed.txt" >> "$work/swept"
    shard=$((shard + 1))
  done

  # Its own process, so that `set -e` holds inside it: in a condition, bash
  # suspends errexit for everything the function runs, and a failing `gh`
  # halfway through would carry on regardless.
  if ! "$0" survivors "$work/swept"; then
    broken "The sweep completed, but filing what it found failed."
    return 1
  fi
  recovered
}

command=${1:-}
[ $# -gt 0 ] && shift
case $command in
  sweep) sweep "$@" ;;
  survivors) survivors "$@" ;;
  broken) broken "$@" ;;
  recovered) recovered ;;
  *)
    echo "usage: $0 sweep|survivors|broken|recovered ..." >&2
    exit 64
    ;;
esac
