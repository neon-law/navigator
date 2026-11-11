---
name: tightbeam-linear-loop
description: >-
  Run a Linear project through the Tightbeam product-owner and delivery-owner workflow, with council review and one
  signed commit per issue.
---

# Tightbeam + Linear: one project, one PR

Tightbeam coordinates the agents; Linear holds the work; GitHub holds the code. The unit of work is a **Linear
project**, delivered as one pull request: one signed Conventional Commit per issue, a `Closes ENG-a, ENG-b, ...`
trailer, and a milestone as the roadmap. Every step defers to the repo's own skills:
[`implement-issue`](../implement-issue/SKILL.md), [`create-pr`](../create-pr/SKILL.md), [`review`](../review/SKILL.md),
[`fix-checks`](../fix-checks/SKILL.md), [`stay-in-repo`](../stay-in-repo/SKILL.md), and the [Rust
guardrails](../rust/SKILL.md).

## Cast

| Who | Tightbeam handle | Owns |
| --- | --- | --- |
| Main | `orchestrator:main` | Picks the project and hires the PDO. Writes no code and stays up. |
| PO | `product-owner:navigator` | Reads the Linear project; rules each issue valid, shipped, duplicate, or blocked. |
| PDO | `pdo:<project>` | Coordinates all after the PO: council, tests, code, review, PR. Retired on delivery. |
| Coder, reviewer | `coder:<project>`, `reviewer:<project>` | One assignment each, answering to the PDO. |

A Product Delivery Owner is the one individual directly responsible for delivering one project. It is not a matter or
Firm [DRI](../../../docs/glossary/directly-responsible-individual-dri.md) (legal-domain accountability stored on
`person_project_role` and `person_firm_role`). Tightbeam has no PDO field: name the PDO in the work item title and the
brief.

## The loop

1. **Read.** The PO reads the Linear project: description, milestone, every issue with its relations and comments. For
   each issue it checks origin/main for already-shipped work and returns a verdict. It hands the PDO a grounded issue
   list in merge order, naming blocked and decision-only issues set aside. The PO edits and opens nothing.
2. **Hire.** Main files one work item for the project (`work-item-create --title "<project> (PDO)" --key <project>`),
   spawns the PDO, and assigns it (`assign` or `dispatch --work-item`). The PDO coordinates; Main watches.
3. **Council.** Before any code, the PDO runs the Engineering Council inline over the project's design and test plan,
   per [`council`](../council/SKILL.md) and
   [`docs/agent-decision-councils.md`](../../../docs/agent-decision-councils.md): read the real source, convene the
   voices, close on one decision. The outcome is a test-first plan per issue: the failing tests, the seams they
   exercise, and the order. A real fork goes to Main, then the user. The council is one response, not twelve sessions.
4. **Code, test first.** The PDO delivers as many issues as it can, in merge order, with full TDD. It hires a coder
   who works in one worktree, signed-rebased on `origin/main`. Per issue, the coder writes the failing tests from the
   council plan, watches them fail, makes them pass, and makes exactly one signed commit, with a `triage-issue` comment
   on the issue before its commit. An issue it cannot finish cleanly gets no commit: the PDO names it as left over, with
   the reason, and moves on. The coder runs `navigator validate` before pushing; the workspace suite is CI's job.
5. **Review.** The PDO hires one fresh reviewer, never the author. The reviewer is read-only: it reproduces each issue's
   claim, checks the diff against the issue and the glossary, and files a verdict on the exact head SHA. Findings go
   back to the coder; the reviewer never pushes. Where a fix changes the design, the PDO re-convenes the council.
6. **Ship.** The PDO runs the changed behaviour for real (`navigator-deploy dev browser-e2e` for UI changes), then opens
   one ready PR per [`create-pr`](../create-pr/SKILL.md) and arms nothing else. The user merges on green CI.
7. **Close.** The PDO attests delivery and lists the issues done and left over. Main retires the PDO, coder, and
   reviewer; Main and the PO stay up for the next project.

## Rules for every brief

- Hand sessions issue text, not Linear access, unless the user has granted a session the Linear connector.
- Public safety: bare `ENG-NN` only, no `linear.app` URLs or project names, no client data. Read
  [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md) first.
- Find the glossary term before naming anything (`navigator-deploy glossary list`); add an entry in `docs/glossary/`
  when none fits.
- Schema changes edit `navigator.surql` and bump `SCHEMA_VERSION` together; diff against `origin/main` first, since two
  PRs can collide silently.
- A failing check, a flaky test, or a surprise is surfaced to Main and then the user, never retried blindly or skipped.

## Operating it

Every command takes `--as-user <user>` and runs from `~`, because the CLI walks up looking for `.tightbeam-session`. A
spawn needs a work item first. Message a session with `tightbeam wake --session <key> --prompt "..."`; read it with
`tightbeam transcript --session <key>`. Check `tightbeam list` for `state: active`, and watch the loop in the ATC
visualizer. Ask before spawning more sessions than the loop calls for, before opening or merging a PR, and before
touching Linear.
