# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --comments`, filtering comments by `jq` and also fetching labels.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

Infer the repo from `git remote -v` — `gh` does this automatically when run inside a clone.

## Publishing a batch of tickets

A spec becomes an epic plus its tickets, and the **structure is the deliverable, not the bodies** — sub-issue links and dependency edges are what the frontier query reads. Two calls do the wiring, and both take the other issue's numeric **database id** (`gh api repos/OWNER/REPO/issues/<n> --jq .id`), never the `#number`:

```sh
gh api --method POST repos/OWNER/REPO/issues/<parent>/sub_issues     -F sub_issue_id=<child-db-id>
gh api --method POST repos/OWNER/REPO/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>
```

**Then read the graph back, and check it against what you meant to write.** Not the responses — the graph:

```sh
gh api repos/OWNER/REPO/issues/<epic> -q .sub_issues_summary.total
gh api repos/OWNER/REPO/issues/<n>/dependencies/blocked_by --paginate -q '[.[].number]'
```

An issue number is a valid database id for *some other* issue, so a wrong-argument write does not necessarily fail — it can succeed against a stranger. And the endpoint **pages at 30**, so a ticket with more blockers than that reads as complete when it is truncated (`--paginate`, always; `issue_dependencies_summary.blocked_by` is the unpaged count to check it against).

**The evidence is #84.** Its programme — an epic and fourteen tickets — was published on 2026-07-30 with a comment stating the chain it had wired and the eleven feature tickets it gated. None of it existed: #84 was a sub-issue of nothing, `sub_issues_summary.total` was `0` on every one of them, and not one dependency edge had been created. The bodies said "Sub-issue of #84" in prose, which is what made it look done. For a day the frontier query returned every ticket that programme was written to gate, and the next `/implement` session would have rebuilt the seams #92 and #96 exist to replace — reported by the tracker as correct. **A publication is not finished until the graph has been read back**, the same way a ticket is not finished until it is closed.

## Starting a ticket

**Pick from the frontier.** The open, unblocked, unassigned children of the version epic — `issue_dependencies_summary.blocked_by == 0`. GitHub's native dependencies carry the order, so nothing else has to:

```sh
gh api repos/OWNER/REPO/issues/<n> -q '.issue_dependencies_summary.blocked_by'
```

**Check it hasn't already landed.** A ticket can be built and still be open — it has happened more than once here (#10 was requested via `/implement` after its commit was already on the working branch). `git log --oneline --grep '#<n>'` before trusting its state.

**Then read it as a claim, not an instruction** — the hard constraint at the top of [CLAUDE.md](../../CLAUDE.md). Before the first test, say what the ticket asserts, what you mean to build, and what you are unsure of:

- **Anything genuinely open → ask, and wait.** A choice between real alternatives goes to `AskUserQuestion`; an answer that will outlive the ticket goes through **`/grill-with-docs`**, which records it in `CONTEXT.md` or an ADR instead of burying it in a commit message.
- **A claim in the ticket that looks wrong is an open question.** Tickets here are written by earlier sessions, often months before the work. They carry inherited reasoning, and #83's §6 carried reasoning that was flatly false — asserted as "known equivalent, do not chase", believed, and written into `.cargo/mutants.toml` as a proof before `/code-review` caught it. Verify the ticket's premises against the code the way you would verify your own.
- **Nothing open → say so, name your assumptions, and build.** Most tickets are like this. A grill on a mechanical ticket spends the maintainer's attention where there is nothing to decide, which is how the grills that matter get rubber-stamped.

**One ticket per session, and a fresh context for each.** `/implement <n>` drives `/tdd` for the build and closes with `/code-review`; the flow assumes it is not sharing a window with the last ticket's reasoning.

## Finishing a ticket

**A ticket is not finished until it is committed, pushed, *and* closed.** All three, in that order, in the session that did the work — never left for "next time". An issue left open after its code shipped is worse than no tracker at all: the frontier query reads open blockers as live gates, so a built-but-open ticket silently blocks everything behind it (#28 sat blocked on an already-finished #27 for exactly this reason), and the next session can't tell "built" from "not started" without reading the diff.

1. `git commit` on the working branch — **`next`**, which takes direct pushes; see CLAUDE.md's branch note for why the release branch is not it — then `git push`. The ticket number goes in the commit subject (`feat(x): … (#27)`). No pull request per ticket: a whole version lands on `master` as one PR when it is complete, which is also the only moment the patch-coverage gate runs, so the local gates are what hold each ticket to the policy.
2. `gh issue comment <n>` with what actually shipped: the commit SHA, which acceptance criteria are met, any decision taken along the way, and anything deliberately left to a later ticket.
3. `gh issue close <n>`.

If the work is genuinely partial, say so in the comment and leave it open — but then say *what* is missing, so the next session doesn't have to re-derive it. "Built but unverified" is not a reason to leave it open; verify it, or write down what verification is outstanding.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments` then keep only `authorAssociation` of `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE` (drop `OWNER`/`MEMBER`/`COLLABORATOR`).
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either — resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api` on the sub-issues endpoint). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies** — the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only — the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed. Read every edge back after writing it — see [Publishing a batch of tickets](#publishing-a-batch-of-tickets) for what a silently-unwritten graph costs.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me` — the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
