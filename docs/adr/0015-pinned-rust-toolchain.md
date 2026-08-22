# Pin the Rust toolchain, and let a bot propose the bumps

## Context

On 2026-08-21 CI went red on `next` and stayed red for four days, across four
pushes, with **no commit responsible for it**. The last green run was
2026-08-18.

Rust 1.98.0 was released on 2026-08-18. Every workflow installs
`dtolnay/rust-toolchain@stable`, and the repository pinned nothing, so the
first push after GitHub's runners picked up the new release inherited two
clippy lints that did not exist when the code was written:

| Lint | Site | Written |
| --- | --- | --- |
| `clippy::drain_collect` | `src/instance.rs:404` | #93, 2026-08-01 |
| `clippy::chunks_exact_to_as_chunks` | `src/enhance.rs:1208` | #20, 2026-07-27 |

`cargo clippy --all-targets -- -D warnings` is a **hard gate** (ADR-0011,
CLAUDE.md), so two new opinions became a blocked branch.

The failure mode is worse than the four days it cost, because of what it did to
the local ritual. CLAUDE.md tells a contributor to run clippy before a commit
lands, "because a red gate is cheaper to find here". Through the whole of #55
that ritual was run, passed, and reported green — on **1.96**, which cannot
see either lint. The local gate was not wrong; it was answering a different
question from the one CI would ask, and nothing about it could say so. A gate
that can be green locally and red remotely for reasons neither side can see is
not a gate, and no amount of care fixes it.

## Decision

**One Rust version, declared in `rust-toolchain.toml`, binding everywhere.**

```toml
[toolchain]
channel = "1.98.0"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
```

rustup honours this file for any `cargo` run inside the repository, so it binds
a contributor's laptop, every CI job, and the `rust:alpine` container the musl
release builds run in. "Clippy is clean locally" now means CI will agree,
which is the only property that makes a pre-commit gate worth running.

**Bumps arrive as a Renovate pull request**, never automatically. `renovate.json`
enables the native `rust-toolchain` manager with `automerge: false`: a new
stable brings new lints, and the right moment to meet them is a reviewable PR
with CI already run against it, not a red default branch on a Tuesday.

Two mechanics are load-bearing and easy to get wrong:

- **The components must be in the file.** `dtolnay/rust-toolchain@stable`
  installs its `components:` onto whatever `stable` is, then runs
  `rustup default` — which is *lower* precedence than a toolchain file. So
  `cargo clippy` resolves to the pinned toolchain, rustup auto-installs it, and
  without the list above it arrives without clippy. The workflow input still
  reads as if it were doing the work; the file is what actually does it.
- **Targets are added by `rustup`, not by the action**, for the same reason and
  in the same direction. The cross-building jobs run `rustup target add` in the
  workspace so the target lands on the pinned toolchain. They are deliberately
  *not* listed in the file: every job would then download the standard library
  for all seven release targets to build one.

## Consequences

- **A Rust release can no longer redden the branch.** It can only open a PR.
- **A contributor on a different stable gets 1.98.0 downloaded** on their first
  build here. That is the cost of the guarantee, and it is paid once per bump.
- **The release container stops using whatever Rust `rust:alpine` ships**, and
  builds the shipped artifact with the pinned compiler instead. This is a change
  to the release path, and on balance the right one: the binary an operator runs
  on a Pi should not depend on when the base image was last rebuilt. It costs a
  toolchain download inside that container.
- **New lints stop arriving for free.** This is the real cost, and it is
  accepted rather than mitigated: clippy's new lints are usually worth having,
  and pinning defers them until somebody merges the bump. The Renovate PR is
  what keeps that from being indefinite. If the deferral turns out to bite, the
  cheap answer is an advisory `clippy @stable` job on `nightly.yml` — annotating,
  never blocking, the way every other advisory signal here works.
- **Renovate must be installed on the repository** for any of the bump half to
  happen. `renovate.json` is inert until it is; Dependabot is not an
  alternative, as it has no `rust-toolchain.toml` support.
- **The version now lives in exactly one place.** `tests/ci.rs` pins the shape
  of the pipeline; nothing pins the toolchain, because there is only one string
  and a second copy is what this ADR exists to prevent.

## Alternatives considered

- **Pin only the workflows** (`dtolnay/rust-toolchain@1.98.0`). Reproducible CI,
  no imposition on contributors — and it recreates this exact incident inverted:
  local and CI on different clippy versions, with CI now the frozen one and a
  contributor ahead of it seeing errors CI never will. Rejected: the divergence
  *is* the bug.
- **Stay floating and fix forward.** Free lints, at the price of the branch
  going red on somebody else's schedule. That is what just happened.
- **Float, but drop `-D warnings` from the required check.** Decouples "the code
  is broken" from "clippy learned a new opinion", and gives up the lint gate
  ADR-0011 relies on. Rejected: the gate is load-bearing.
