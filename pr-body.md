## Why

The LAW-177 card asked for a `CLIENT_DRI` special question: templates write `{{client_dri}}` and the Project fills it — no template hard-codes the client's name, and no questionnaire re-asks a fact Navigator already holds. Navigator already knows who a matter's designated client decision-maker (DRI) is (the participation model has `is_client_dri`); that fact was never reachable from template bodies or Clauses.

## What

A Project can now pin its designated client DRI as a Person, and that Person's identity flows into template rendering and Clause bindings without a question being asked:

- **Store** (`f8eb4b6b`): a `client_dri_person_id` column on the Project — SurrealDB source schema (bumped to 113), the checked-in PostgreSQL `schema.sql`, a registered migration (`20261108000000_add_project_client_dri.sql`), the PostgreSQL mirror's columns/conversions/upsert bindings, and read plumbing (`PROJECT_SELECT` carries the column). `set_client_dri_person_id` fails closed: it refuses unknown People and People without an `is_client_dri` participation on that same Project, and clears back to `None`.
- **Rules** (`577bc770`): person-typed clause inputs may bind `project.client_dri` directly and expand to the pinned Person's fields (`client_dri_contact@1` in the library exercises it); the f115 template grounding recognizes `project.client_dri` and the `client_dri.<field>` alias — still refusing unknown fields and paths.
- **Portal** (`363776bc`): `build_answer_context` seeds `client_dri.name` / `client_dri.email` and the `project.client_dri` expansions from the pinned Person in both the keys and labels contexts, with questionnaire answers taking precedence (`or_insert_with`); an unpinned Project fills nothing rather than an empty string.

This follows the LAW-228 (`onboarding_jurisdiction`) playbook, with the workflow-side resolution landing in the render context (the same seam that seeds `project.*` values) rather than `answer_value_for_state`.

## How to Review

- The fails-closed behavior is the core invariant: `store/tests/project_client_dri.rs` pins that a Person without the role, a Person from another matter, and an unknown id are all refused, and that the pin clears.
- The render-context test splits pinned (`the_bound_client_dri_fills_its_render_context_without_a_question`) from unpinned (`an_unpinned_project_fills_nothing_for_the_client_dri`) coverage.
- Regenerated PostgreSQL artifacts (`caller-inventory.json`, `source-call-sites.jsonl`, `postgres-erd.mmd`, `surreal-source-erd.mmd`, `parity-manifest.json`, `schema.sql`) are folded into the store commit; `postgres_callers check` passes and `postgres_schema check` reports only the 77 schema + 101 seed-coverage differences that pre-exist on clean `main`.
- Backend-only change: visual evidence is not applicable.

## Verification

- `cargo test -p store --test project_client_dri` — 7/7 pass.
- `cargo test -p rules clauses::tests::` — 17/17; `f115` — 27/27; full rules library suite 994/994 (pre-rebase).
- `cargo test -p portal --lib the_bound_client_dri` and `an_unpinned_project` — 1/1 each, after the rebase onto `origin/main` at `215cc0bd`.
- `cargo clippy` clean for store (lib+tests), rules (lib+tests), portal (lib+tests); `cargo fmt --all -- --check` clean.

Human author: Nicholas Shook (nick@neonlaw.com)

Closes LAW-177.

🔗 [Obvious Project](https://app.obvious.ai/p/law-issue-implementation-bB6RqZbU) · 🧵 [Obvious Thread](https://app.obvious.ai/p/law-issue-implementation-bB6RqZbU?thread=th_ZDqf6UsZ) · 🔀 [Obvious PR Artifact](https://app.obvious.ai/prs?prRef=review-the-law-issues-and-implement-10-of-them-tha-task:todo_1xcOZlaZ)
