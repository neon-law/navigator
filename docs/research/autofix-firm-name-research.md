# LAW-300 research: the Project gate's autofix rewrites the Firm's name to "the Firm" in letters

- **Linear:** LAW-300 (read-only research; task `todo_osdMEwdn`)
- **Researched against:** `origin/main` at `98125578` ("feat(store): persist the fixed synthetic Sun lower
  meridian (#571)") — later than the pinned `113fcc7b` floor named in the task
- **Branch:** `task/research-law-300-gate-autofix-rewriting-osdMEwdn`
- **Scope:** research only. No source files were edited; this document is the only file on the branch.
- **Date:** 2026-11-08

## Expected vs. observed behavior

- **Expected:** the gate's autofix pass never changes a client-facing letter's identification of the Firm —
  the signature block prints the Firm's real legal name (via the `{{firm.legal_name}}` render binding), and
  the opening line names the Firm as authored.
- **Observed:** a local `navigator project gate` run rewrites the Firm's legal name to the literal string
  `"the Firm"` in a letter's opening line and (in the failure modes below) in its signature block, and writes
  the corrupted letter back to disk.

## Verdict up front

The rewrite is **not a placeholder token gone wrong and not inverted logic** — it is a working de-branding
rule doing exactly what it was built to do, on a surface it was never meant to touch. Rule `Y027`
(`FIRM_ROLE_NAME_CODE`, `cli/src/projects/repository.rs:2682`), introduced by commit `3f09bd70` ("Name the
Firm by role and resolve its legal name from the brand (#395)", 2026-11-04, `Closes LAW-272`), replaces every
compiled Firm legal name in Project prose with the role name `"the Firm"`, and its local gate run persists
the rewrite with `fs::write`. Letters — `drafts/<code>.md` blueprints with `kind: letter` — are linted by the
same path with no kind-aware exemption, and the signature-block detection that is supposed to swap in
`{{firm.legal_name}}` instead of `"the Firm"` misses the most common letter closing formulas, so a
signature line can fall through to the prose branch and become `"the Firm"` in a letter that will print.

## 1. Where the autofix rewrites letter markdown

The rewrite is **not** one of the `rules::Rule` autofixes (the M-family whitespace edits consumed by
`fix_directory`, `cli/src/main.rs:9207`). It is a dedicated repository check with its own write path:

- `Y027` is defined at `cli/src/projects/repository.rs:2682` (`FIRM_ROLE_NAME_CODE`). Its doc comment
  (2678-2681) states the design: *a compiled Brand's legal name in a skill, `README.md`, `AGENTS.md`, or a
  Draft is a warning, and a local gate rewrites it. A signature block or From line uses
  `{{firm.legal_name}}`.*
- The line rewriter is `rewrite_firm_role_line` (`repository.rs:2800-2819`). For each compiled legal name
  found on the line it substitutes either `"the Firm"` (the prose branch) or `"{{firm.legal_name}}"` — the
  latter only when the line is inside a detected signature section (`in_signature`), prints only the name
  (`line_prints_only_the_name`, 2735), or is a `From:` line (`from_line`, 2742).
- The document rewriter is `rewrite_firm_role_prose` (`repository.rs:2822-2847`). It computes `draft` as
  "frontmatter declares a `kind:`" (2826-2828), marks named-entity fact paragraphs as protected
  (`named_entity_fact_lines`, 2751), and walks signature sections via `signature_line` (2774-2798). There is
  **no protection for a letter's salutation or opening line** — only fact/definition paragraphs are exempt.
- `apply_firm_role_prose` (`repository.rs:2865-2889`) is the write point. With `write_fixes = true` it calls
  `fs::write(path, rewritten)` (2877) and prints `fixed … commit the rewrite` (2878-2881); otherwise it emits
  `Y027` warnings via `firm_role_findings` (2849-2863, 2882-2888).
- Two callers feed it:
  - `validate_firm_role_prose` (`repository.rs:2891-2912`) — `AGENTS.md`, `README.md`, `.agents/skills/**.md`.
  - `lint_project_template` (`repository.rs:2914`, call at 2982) — invoked from `validate_templates`
    (1940, called at `validate_gate` 429-438) for **every direct `drafts/<code>.md` blueprint**. A letter
    template reaches the rewrite here, with no exemption by `kind:` at any layer.

### The opening-line corruption

Any opening sentence carrying a compiled legal name — `Guieb & Shook PC is pleased to represent you` — hits
the prose branch and becomes `the Firm is pleased to represent you`. The paragraph protections
(`states_named_entity_fact`, 2702-2715; `defines_the_firm`, 2717-2733) cover conversion/shareholding/
services-agreement/trademark facts and defined-term paragraphs, never a salutation or engagement opening.

### The signature-block corruption

`signature_line` (2774-2798) opens a signature section only on:

- a heading containing `signature`, `acceptance`, or `execution`;
- a line whose de-emphasized label is exactly `sincerely,`, `respectfully,`, or `agreed and accepted:`, or
  starts with `in witness whereof`;
- a line starting with `by:`, `signed by:`, or `signature:`.

A letter closing with `Yours truly,`, `Very truly yours,`, `Warm regards,`, or `Kind regards,` opens no
section. The Firm-name line under such a closing then escapes the placeholder branch: it is rewritten to
`"the Firm"` unless it is a bare lone-name line. A signature line that carries any other text
(`Guieb & Shook PC, Attorneys at Law`) or a one-line closing-plus-name (`Yours truly, Guieb & Shook PC`)
corrupts exactly as LAW-300 reports.

The repo's own integration test
(`cli/tests/project_repository.rs:1634`,
`gate_autofix_preserves_the_firm_identity_in_a_definition_and_signature`) pins the *working* variant: a
signature line introduced by a `## Signatures` heading becomes `For {{firm.legal_name}}:`. The test never
exercises an unrecognized closing formula, which is why the gap ships green.

## 2. Where the rewrite source comes from, and why the real name is replaced

- **Source of the names searched for:** `compiled_firm_legal_names` (`repository.rs:2686-2700`) maps every
  key of `views::brand::BrandKey::ALL` (`views/src/brand.rs:1014`) through `resolve_branding(&DEFAULT_BRANDING)`
  and collects `firm.legal_entity`. The compiled registry resolves to exactly two names — `Guieb & Shook PC`
  (`views/src/brand.rs:472` et seq.) and `Lex Tecnica, Ltd.` (`LEX_TECNICA_LEGAL_ENTITY`; also
  `store/src/seed.rs:128`/`135`, where both are `FIRM_ENTITY_NAMES`). Longer-first ordering avoids rewriting
  one name inside another.
- **Source of the replacement:** a hardcoded role name, `"the Firm"` (`repository.rs:2812`), or the render
  binding `"{{firm.legal_name}}"` (`repository.rs:2810`). Neither comes from the Project's `navigator.yaml`
  and neither from a runtime brand row.
- **Why the real name is replaced:** it is the rule's purpose. `Y027` is the LAW-272 de-branding rule —
  commit `3f09bd70` says plainly: *"Repo prose names the Firm by role… The project gate warns when a compiled
  Firm legal name appears… A local run rewrites it."* There is no missing brand record and no fallback
  default involved in the prose branch; the literal `"the Firm"` is the intended output. The corruption is
  (a) design overreach — the rule runs over client-facing letter blueprints with no kind-aware exemption —
  and (b) detection gaps — the signature branch that is supposed to preserve the printable name misses
  common closings.
- **LAW-254-shaped second failure mode (navigator.yaml brand absence):** where the rewrite *did* substitute
  `{{firm.legal_name}}`, the placeholder is only bound at render time by `fill_firm_legal_name`
  (`cli/src/main.rs:8764-8777`) → `project_firm_legal_name` (`main.rs:8779-8792`) → `letterhead::for_project`
  → `resolve_brand` (`cli/src/letterhead.rs:93-112`). A `navigator.yaml` that declares neither `brand` nor
  `host` makes `resolve_brand` return `LetterheadError::Undeclared` (letterhead.rs:102),
  `project_firm_legal_name` returns `None`, and the placeholder stays unbound: a `--preview` render prints
  the literal `{{firm.legal_name}}`, and a final render refuses with "a final render carries no unfinished
  text" (`rules/src/final_document.rs:7,122`). So the rule can leave a letter that cannot finish rendering.
  I could not retrieve LAW-254's exact text from this sandbox (no Linear access here; the identifier is not
  referenced in the repo) — flagging that mapping as inferred, not verified.

## 3. Relation to LAW-272 and LAW-266

- **LAW-272 — this is its implementation.** `Y027` and the rewrite machinery landed together in commit
  `3f09bd70` (PR #395, "Closes LAW-272"). The repo-prose half (AGENTS.md, README, skills) behaves as
  specified. The Draft half is where LAW-300's symptom lives: the commit message's contract — *"A letterhead,
  From line, or signature block writes `{{firm.legal_name}}`, and render fills that from the Firm behind the
  Brand declared as `project.brand` in `navigator.yaml`"* — holds only when `signature_line` recognizes the
  block and when the manifest resolves a Brand.
- **LAW-266 (runtime brand rows) — touches the render path, not the gate path.** The letterhead seam already
  prefers the deployment's stored brand rows (`cli/src/letterhead.rs:172-221`, `for_brand_runtime`;
  `store/src/brand_letterheads.rs:96`, `firm_legal_name`, populated from `brand.legal_entity` at 293) with
  the compiled registry as announced fallback, and an unknown Brand is a refusal, never a default. Nothing in
  the `Y027` rewrite path consults runtime rows or `navigator.yaml` — the gate decides purely from the
  compiled registry. The two surfaces meet only downstream, at render.

## 4. Minimal verdict-neutral fix candidates

Severity context for each: `Y027` findings are warnings and never fail a gate run
(`apply_firm_role_prose` pushes to `warnings` only; `validate_gate`'s outcome counts `errors`, and the CI
invocation writes nothing — see §6). So every candidate below is verdict-neutral by construction: gates that
pass today keep passing.

### A. Exempt letter-kind drafts from the prose rewrite (recommended)

Read the frontmatter `kind:` (already parsed at 2826-2828) and, for client-facing letter kinds, restrict the
rewrite to the placeholder branch — signature sections and `From:` lines only — skipping the `"the Firm"` prose
substitution.

*Blast radius:* letters stop being de-branded in prose; the signature → `{{firm.legal_name}}` migration is
preserved. Repo prose, skills, and non-letter drafts keep today's behavior. The canonical letter templates
(`templates/notations/neon_law/*letter*.md`) already use `"the Firm"` prose and placeholder signatures, so the
pinned test at `repository.rs:5940` still passes untouched. The smallest change that fixes both corruption
modes.

### B. Extend `signature_line`'s closing formulas

Add `yours truly,`, `very truly yours,`, `warm regards,`, `kind regards,`, and `best regards,` to the label
list at `repository.rs:2792-2795`.

*Blast radius:* tiny and surgical, but formula-based detection stays fragile — the next unusual closing
re-corrupts. Worth doing with A as defense in depth; insufficient alone.

### C. No-op the rewrite when the Project's Brand does not resolve

Have the gate read `navigator.yaml` (it already parses it elsewhere) and, when `resolve_brand` fails
(`Undeclared`/`UnknownBrand`/`UnknownHost`), skip the rewrite and keep the `Y027` warning naming the
unresolvable brand.

*Blast radius:* converts silent corruption plus an unbound placeholder into a loud warning. Does **not** fix
the opening-line de-branding for well-formed projects, and introduces a manifest read into the rewrite path.
Pairs naturally with A.

### D. Resolve the brand from the live Project before rewriting

Consult runtime brand rows (the LAW-266 surface) in the gate.

*Blast radius:* rejected for now — it puts a network dependency into a local offline-first gate for no
rewrite-accuracy gain; the rewrite already targets names, not brand identity, and the render seam already owns
runtime resolution.

### E. Stop persisting (warn-only `Y027`)

Drop the `fs::write` branch.

*Blast radius:* rejected — it reverses the deliberate LAW-272 design that a local run repairs repo prose, and
it changes gate UX for every existing run, not just letters.

Recommendation: **A + B** (optionally C). A fixes the class of letter corruption at its root — the missing
kind-aware boundary between repo prose and client-facing work product — rather than widening a detection
formula. B closes the residual signature gap for non-letter drafts. C is the right follow-up only if LAW-254
confirms manifests may legitimately omit a brand; that is a user-owned call.

## 5. Test surface to extend

- `cli/src/projects/repository.rs:5921` — `firm_role_prose_rewrites_a_role_and_leaves_a_named_entity_fact`:
  the unit pin for `rewrite_firm_role_prose`; extend with a `kind: letter` frontmatter case asserting prose
  names survive and signature/`From:` lines still become the placeholder.
- `cli/src/projects/repository.rs:5940` — `the_shared_contract_and_letter_templates_name_the_firm_by_role`:
  asserts the canonical letter templates are rewrite-clean; keeps them honest under A.
- `cli/tests/project_repository.rs:1634` —
  `gate_autofix_preserves_the_firm_identity_in_a_definition_and_signature`: the end-to-end gate autofix pin;
  add an unrecognized-closing variant (`Yours truly,` + name line) asserting no `"the Firm"` substitution in
  the signature region.
- `cli/tests/gate.rs` and `cli/tests/project_gate_pinned_release.rs` — existing gate-level autofix harnesses;
  the right home for a `navigator project gate` (no `--ci`) run asserting the letter file on disk is
  byte-identical after the pass (persistence semantics, §6).

## 6. Is the corruption persisted, or only suggested?

**Persisted — by the gate run itself.** `navigator project gate` without `--ci` calls `validate_gate` with
`write_fixes = !ci` (`cli/src/main.rs:7829`), and `apply_firm_role_prose` writes the rewritten letter to disk
(`repository.rs:2877`), printing `fixed … commit the rewrite`. The corrupted bytes are the new working-tree
content the operator is invited to commit. Under `--ci` — what a Project repository's CI actually runs
(`.github/workflows/project-gate.yml:103`, `navigator project gate --ci`) — the fix is computed but nothing
is written (per the comment at `main.rs:9292-9296` and the workflow's own note at lines 97-99), and the
`Y027` finding is a warning that never fails the run (`LayoutOutcome` counts errors only;
`main.rs:7830-7846`). So CI cannot introduce the corruption, but it also cannot catch it: a letter corrupted
locally passes CI cleanly. Executed and filed letters are out of reach — `documents/` is excluded from the
gate's markdown walk (`ProjectGateMarkdownFilter`, `main.rs:7894-7915`), so only blueprint sources in
`drafts/`/`templates/` are at risk. Severity: **high for authored letters** — silent local destruction of
client-facing work product that CI is structurally blind to.

## User-owned questions (flagged, not decided)

1. **Should letter prose ever say "the Firm"?** The canonical letter templates deliberately use role prose
   (`Thank you for choosing the Firm`, `templates/notations/neon_law/onboarding__letter.md:77`), so LAW-300's
   opening-line complaint is partly a product-preference question, not purely a defect. Candidate A as
   specified stops *rewriting* letters but changes nothing about templates authored in role prose today.
2. **LAW-254's exact scope.** Whether a `navigator.yaml` without a resolvable Brand is an authoring error to
   refuse (candidate C) or a legitimate state to tolerate is the premise candidate C rests on; the issue text
   was not retrievable from this sandbox.
3. **Whether the two-firm compiled name list is right for white-label Projects.** `compiled_firm_legal_names`
   mixes `Guieb & Shook PC` and `Lex Tecnica, Ltd.` into one search list; a Lex Tecnica Project's letters get
   the Guieb & Shook treatment and vice versa. Probably intended (templates are shared), but worth an
   explicit nod from the issue owner.

## Evidence index

Every material claim above, with its source at `98125578`:

- `Y027` definition and design comment — `cli/src/projects/repository.rs:2678-2682`
- Prose vs. placeholder branch — `cli/src/projects/repository.rs:2800-2819`
- No salutation protection; fact-only exemption — `repository.rs:2751-2769` and `2702-2733`
- Signature-section detection formulas — `repository.rs:2774-2798`
- `fs::write` persistence and "commit the rewrite" — `repository.rs:2865-2889` (write at 2877)
- Draft/letter lint entry — `repository.rs:2982` via `validate_templates:1940` via `validate_gate:377,429-438`
- Repo-prose entry — `repository.rs:2891-2912`
- Local gate writes, CI does not — `cli/src/main.rs:7829` (`write_fixes = !ci`);
  `.github/workflows/project-gate.yml:97-103`
- `Y027` is warning-only, never fails a run — `repository.rs:2865-2889` (warnings sink);
  `main.rs:7830-7846` (errors-only exit)
- Name list = compiled registry, two legal entities — `repository.rs:2686-2700`; `views/src/brand.rs:472,1014`;
  `store/src/seed.rs:128,135,142`
- `{{firm.legal_name}}` binding at render — `cli/src/main.rs:8764-8792`; `cli/src/letterhead.rs:93-112`
  (`Undeclared` at 102)
- Runtime brand rows touch render, not gate — `cli/src/letterhead.rs:172-221`;
  `store/src/brand_letterheads.rs:96,293`
- Unbound placeholder: preview prints literal, final render refuses — `rules/src/final_document.rs:7,122`;
  `main.rs:2535-2537`
- Working-behavior end-to-end pin (recognized signature heading) — `cli/tests/project_repository.rs:1634-1681`
- LAW-272 introduction — commit `3f09bd70`, PR #395, 2026-11-04, "Closes LAW-272"
- Canonical letters already in role prose — `templates/notations/neon_law/onboarding__letter.md:77,232`;
  `engagement_letter__nevada.md:175`
- `S101` is an Error-severity, 120-character rule that only exempts fenced code blocks —
  `rules/src/lib.rs:530-575` and `severity_for_code` at `rules/src/lib.rs:503-516`
