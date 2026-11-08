# LAW-299 research — the scaffolded onboarding stub fails its own gate (N110, `jurisdiction:`)

**Researched against:** `origin/main` at `65a2c900` ("style(web): make the shared public navigation full
width (#569)"), on branch `task/research-law-299-scaffold-onboarding-stu-YUcjqve6`. The brief's last-known
base `82a4a74` predates two commits that change this issue's verdict — see the headline below. All
`file:line` references below are to this checkout unless noted.

## Headline: LAW-299 is already fixed on `origin/main`

Commit `113fcc7b` — `fix(cli): accept the generated onboarding stub in the Project gate (#565)`, authored
2026-11-08, message ends `Closes LAW-299` — merged today, one commit after the brief's base. It implements
exactly the "N110 skips the unedited onboarding stub" candidate (§4, option B). `origin/main` also carries
`164be2d0` ("fix(cli): explain the terminal requirement for Project scaffold (#563)"), a neighboring
scaffold UX fix. Everything below maps the failing path as it stood at the brief's base, verifies the fix,
and prices the remaining alternatives.

## 1. What `project scaffold` writes, and which generator produces it

- The forge-agnostic file set lives in the `project-scaffold` workspace crate
  (`Cargo.toml:5` lists `project-scaffold`). `canonical_files` (`project-scaffold/src/lib.rs:192-210`)
  emits four files: root `README.md`, root `.gitignore`, the `navigator.yaml` manifest, and the onboarding
  letter Draft placeholder — `(draft, stub)` from `scaffold_draft(ONBOARDING)` at line 195.
- `scaffold_draft` (`project-scaffold/src/lib.rs:461-470`) returns the path
  `drafts/onboarding__letter.md` (via `rules::project_blueprints::BlueprintDirectory::Drafts` and
  `template_code`, which maps `ONBOARDING` to `"onboarding__letter"` at lines 474-483) paired with
  `template_stub(kind)`.
- `template_stub` (`project-scaffold/src/lib.rs:485-509`) is the stub bytes. Frontmatter is
  `kind: onboarding`, `title: Onboarding letter`, `respondent_type: person_and_entity`, `confidential: true`
  — then, only for onboarding, a questionnaire chain (`BEGIN → jurisdiction__governing_law →
  jurisdiction__arbitration_seat → END`), an `engagement_arbitration@1` Clause binding
  `law: jurisdiction__governing_law` / `seat: jurisdiction__arbitration_seat`, and a workflow
  (`created: lawyer_review`, `approved: END`). The body is the placeholder line
  "Draft this letter before running `navigator project onboard` again."
  (`template_placeholder`, lines 511-521).
- **The gap:** the stub's frontmatter declares *question* states named `jurisdiction__*` but has **no
  top-level `jurisdiction:` metadata field**. That is the entire defect.

## 2. What N110 checks, where it is implemented, and its exact input

- Rule file: `rules/src/f110.rs`, `F110JurisdictionPath`, `CODE = "N110"` (line 89). Doc header (lines
  1-19): N110 is "notation templates must live on the shelf their tree owns **and declare their
  jurisdiction**".
- Inputs: **only the template file itself** — its path (shelf layout) and its frontmatter. Not
  `navigator.yaml`, not a store record, not a scaffold flag.
  - Layout half (`lint`, lines 113-186): in a Project repository tree (detected by walking ancestors for
    a `navigator.yaml` with a `project` key, `is_project_repository_tree`, lines 46-63), a template must be
    a direct `templates/<code>.md` or `drafts/<code>.md` file; in Navigator's catalog it must sit under
    `notations/neon_law/` or `notations/forms/`.
  - Jurisdiction half (lines 174-203): frontmatter must exist — a missing block fails with "Missing
    frontmatter (file must declare `jurisdiction:`)" — and `frontmatter::field(fm, "jurisdiction")` must
    be non-empty, else `"Frontmatter is missing required `jurisdiction:` field"` (line 193). The value must
    be a code seeded in `store/seeds/Jurisdiction.yaml` (checked against the compile-time
    `JURISDICTIONS` table, lines 30-43). The only exemption is `kind: resume` (lines 187-192). A
    `kind: onboarding` stub gets no exemption, so the scaffold's own bytes fail N110 the moment the
    Project gate first runs. N110 is an **Error**, not auto-fixable (`docs/gate.md:252`).
- Note on the issue title: LAW-299 cites "N110, no `jurisdiction:`", which matches this rule — the
  shelf wording in the doc header is newer than the jurisdiction requirement, but both halves live in
  `f110.rs` under code N110.

## 3. Do LAW-254 (navigator.yaml brand) or LAW-228 (onboarding_jurisdiction) touch this surface?

- **LAW-254 brand field — no.** `brand` exists in the manifest surface (`cli/src/projects/manifest.rs:73`
  allowlist, `:156` field, firm/brand cross-check at lines 191-203 and 623-625). It validates Firm/brand
  ownership against the live Project and never feeds N110.
- **LAW-228 `onboarding_jurisdiction` — adjacent, already shipped, but a different surface.** The live
  Project record carries `onboarding_jurisdiction` (`portal/src/openapi.rs:5341,5475`; set via
  `navigator project set --jurisdiction`, `cli/src/projects/set.rs:396-398`; flag docs at
  `cli/src/main.rs:1291-1296`). `rules/src/clauses.rs:157-168` allows a Clause input to bind directly to
  `project.onboarding_jurisdiction` (rendered as `{{project.onboarding_jurisdiction}}`,
  `rules/src/clauses.rs:766-779`), and `rules/src/f115.rs:356-360` resolves that binding. The glossary
  documents the intent (`docs/glossary/project.md:112-115`). **But N110 reads the letter file's own
  frontmatter `jurisdiction:` field, not the Project record**, so LAW-228's field does not satisfy N110
  for an un-drafted stub. It *is* the natural source if the product later wants scaffold to default the
  frontmatter (§4, option A).

## 4. Fix candidates, verdict-neutral, with blast radius

**Option B — the gate exempts the exact unedited stub (implemented by #565).**
`cli/src/main.rs:7858-7887` (`fix_gate_markdown`, added by `113fcc7b`): when the tree is a Project
repository, the gate regenerates the canonical stub with
`project_scaffold::scaffold_draft(project_scaffold::ONBOARDING)` and, only when a walked file's path and
contents are byte-identical to it, retains every rule except N110
(`checks.retain(|rule| rule.code() != rules::F110JurisdictionPath::CODE)`). Authored letters are untouched:
the same commit's test (`cli/tests/project_repository.rs:967-988`) proves the gate fails with N110 the
moment the placeholder line is replaced with real prose. Blast radius for **existing** projects: only a
project still carrying the byte-exact unedited stub changes verdict — from failing to passing, which is
the point; every authored or edited letter still needs `jurisdiction:`. Residual fragility: the exemption
is byte-identity, so any autofix or editor touch that alters one byte of the stub reintroduces the N110
error until the letter is drafted — defensible, since `is_unedited_stub`
(`cli/src/projects/lifecycle.rs:286-294`) uses the same equality for `project onboard`'s refusal to open a
notation from the stub (`lifecycle.rs:65`, `:231`), so gate acceptance and onboard refusal stay aligned.

**Option A — scaffold asks for or defaults the engagement jurisdiction.** Scaffold could prompt (or take a
flag) and write `jurisdiction: NV` into the stub. Blast radius: touches the scaffold CLI surface and every
existing test that scaffolds without a jurisdiction argument; and the engagement jurisdiction is often
unknown at scaffold time (it may come from the client's matter, cf. the separate `--jurisdiction` that
`project onboard` needs for the client's Human Entity, `cli/src/main.rs:1291-1296` and
`cli/src/projects/scaffold.rs:98`). A default from the Project's `onboarding_jurisdiction` record (LAW-228)
is only available if that field is set before scaffold, which inverts the natural order. **Flag: whether
scaffold should collect engagement jurisdiction is a product/scaffold-prompt decision — user-owned.**

**Option C — the stub template satisfies N110 (scaffold writes `jurisdiction:`).** A hardcoded value would
be a lie (the letter has no engagement terms yet); N110 would then demand a seeded code for a jurisdiction
nobody chose, and every scaffolded project would carry the same wrong code. Not recommended; noted for
completeness.

**Option D — N110 exempts `kind: onboarding` stubs by shape.** Weaker than B: it keys on `kind`, so a
half-drafted letter that still says `kind: onboarding` would escape the jurisdiction requirement. B's
byte-identity is the tighter gate.

## 5. Test surface that verifies it

- **The fix's own test:** `the_gate_accepts_the_generated_onboarding_stub_and_requires_jurisdiction_after_drafting`
  (`cli/tests/project_repository.rs:967-988`) — scaffolds, writes the regenerated stub, asserts the
  offline gate reports `0 error(s)`, then replaces the placeholder with prose and asserts N110 +
  "missing required `jurisdiction:` field". This is the family to extend for any rework.
- **Helpers:** `scaffold()` (`cli/tests/project_repository.rs:268`), `gate()` (`:535`),
  `offline_gate_is_clean()` (`:542` — offline runs exit 1 with `live Project check is inconclusive` but
  must report `0 error(s)`).
- **Rule unit tests:** `rules/src/f110.rs` (`flags_missing_jurisdiction` at line 378,
  `flags_form_code_missing_jurisdiction_prefix` at line 453, plus the layout tests) — extend here for any
  N110 semantics change.
- **Other families:** `cli/tests/gate.rs:532` (catalog N110), `cli/tests/jurisdiction_reconciliation.rs`
  (every template's jurisdiction is seeded — guards the `JURISDICTIONS` table), `cli/tests/project_setup.rs`
  (scaffold E2E), and `project-scaffold/src/lib.rs` tests (`template_stub` shape, lines 699-712).

## 6. Workflow-YAML and server-side involvement

- `.github/workflows/project-gate.yml` is the reusable gate a Project's own `ci.yml` calls, pinned to an
  exact release tag (`project-gate.yml:1-11`); it runs the CLI gate plus live read-only checks and seed
  dry-runs. So a scaffolded project's first PR failed CI on N110 — the bug was user-visible in their CI,
  not only locally.
- Server-side: none for N110 itself. The gate's `live_status` (`cli/src/projects/gate.rs:72`) is the
  server round-trip and is orthogonal to the markdown rule pass; N110 needs no network, navigator.yaml
  field, or store record (§2).

## Recommendations

1. **Accept #565 as the fix; no code change needed from this task.** It is the smallest verdict-neutral
   option (B), matches the existing `is_unedited_stub` semantics, keeps the jurisdiction requirement for
   every authored letter, and its test pins both directions. Trade-off: byte-identity is brittle against
   any mutation of the stub — but the same brittleness already governs `project onboard`, so the surfaces
   agree, and a mutated stub that still carries no jurisdiction *should* fail the gate.
2. **User-owned product question, flagged not decided:** should `project scaffold` collect the engagement
   jurisdiction up front (option A) so the stub is born valid? That changes the scaffold prompt surface and
   the `onboard --jurisdiction` story, and presumes the jurisdiction is knowable at scaffold time. #565
   makes this strictly optional polish.
3. **Keep LAW-228 and this surface separate.** `project.onboarding_jurisdiction` binds Clause inputs at
   render time; it cannot satisfy N110's frontmatter check, and wiring it to would couple a lint rule to a
   live store record — a bigger blast radius than the bug deserves.

## Evidence index (retrieved this session)

| Claim | Source |
|---|---|
| #565 closes LAW-299; diff shape | `git show 113fcc7b` (commit message, `cli/src/main.rs` +44/-12, test, docs) |
| Stub bytes and path | `project-scaffold/src/lib.rs:192-210, 461-521` |
| N110 rule and messages | `rules/src/f110.rs:46-63, 89, 113-203`; `docs/gate.md:252` |
| Jurisdiction codes source | `rules/src/f110.rs:15-43`; `cli/tests/jurisdiction_reconciliation.rs` |
| LAW-228 clause binding | `rules/src/clauses.rs:157-168, 766-779`; `rules/src/f115.rs:356-360` |
| LAW-228 record field | `cli/src/projects/set.rs:396-398`; `portal/src/openapi.rs:5341,5475`; glossary |
| LAW-254 brand surface | `cli/src/projects/manifest.rs:73, 156, 191-203, 623-625` |
| Stub refusal parity | `cli/src/projects/lifecycle.rs:65, 231, 286-294` |
| Test surface | `cli/tests/project_repository.rs:268, 535-542, 967-988`; `rules/src/f110.rs:378, 453` |
| Workflow involvement | `.github/workflows/project-gate.yml:1-11`; `cli/src/projects/gate.rs:72` |
