---
name: implement
description: "Implement a piece of work based on a spec or set of tickets."
disable-model-invocation: true
---

Implement the work described by the user in the spec or tickets.

**Invoke the `tdd` skill** — the Skill tool, `skill: tdd` — before writing any code, and run its red→green loop for one vertical slice at a time. Confirm the seams with the user before the first test, as that skill requires. Writing a failing test from memory is not this step: naming `/tdd` here does not load it, so it has to be invoked.

Run typechecking regularly, single test files regularly, and the full test suite once at the end.

Once done, **invoke the `code-review` skill** (Skill tool, `skill: code-review`) to review the work.

Then finish the ticket properly — **committed, pushed, and closed, all three**:

1. Commit your work to the current branch, with the ticket number in the subject.
2. Push it.
3. Comment on the ticket with what shipped (commit SHA, which acceptance criteria are met, decisions taken, anything deliberately left to a later ticket), then close it.

Leaving a finished ticket open silently blocks every ticket behind it. If the work really is partial, leave it open — but say in the comment exactly what is missing.

Before starting, check whether the ticket already landed (`git log --oneline --grep '#<n>'`); an open ticket is not proof it is unbuilt.
