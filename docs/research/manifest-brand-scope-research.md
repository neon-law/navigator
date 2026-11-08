# LAW-254 research — `navigator.yaml` declares the Project's Brand, and `project create`/`scaffold` take it

**Researched against:** `origin/main` at `c095f636` ("Default GeneralNag to DBOS on enabled deployments (#573)",
2026-11-08), on branch `task/research-law-254-navigator-yaml-brand-sc-svExKJzt`. This satisfies the brief's
floor (`113fcc7b` or later). All `file:line` references are to this checkout. The brief's prior line citations
still hold, with one drift: `for_brand_runtime` now sits at `cli/src/letterhead.rs:178` (the brief's 172-221),
and `resolve_brand` spans `cli/src/letterhead.rs:93-118`.

## Headline: the core identity decision is shipped at three layers; a small scaffold-time remainder exists

Every acceptance item in the 2026-11-05 governing decision is implemented and tested: the manifest accepts
`project.host`/`project.brand`, the host selects the Brand from the compiled registry, a declared brand is a
consistency assertion (contradiction is a refusal), and manifest-vs-live ownership agreement is enforced at
the CLI gate, `project set`, the portal API, and the store. `project create` and `project scaffold` both take
`--firm`/`--brand` (the title's requirement); scaffold copies the opened row's actual ownership into
`navigator.yaml`. All four named failure classes refuse with named causes. No path re-brands an executed or
filed artifact. What remains is scaffold-time posture (host is not checked against the Brand registry when
the tree is written), an interactive prompt that never asks about Firm/Brand, one soft failure in the
questionnaire's Firm-name fill, and two test gaps. Section 6 prices them; §7 flags the user-owned decisions.

## 1. Acceptance items, mapped to implementation

### Declare a registered Brand host

- The manifest's accepted key set carries top-level `host` (`cli/src/projects/manifest.rs:62-75`,
  `ACCEPTED_KEYS`) and `project:`-nested `firm`/`brand` (`manifest.rs:71-86`, `PROJECT_KEYS`). Unknown keys
  are refused by name (Y006); the retired `navigator.yml` spelling is a rename finding, not a second schema
  (`manifest.rs:24-27`, Y008) — the no-filename-rename requirement holds.
- `project scaffold` writes the deployment host as `project.host` in the generated manifest
  (`project-scaffold/src/lib.rs:215`, the manifest template line; host supplied by `write_tree`,
  `cli/src/projects/scaffold.rs:306-324`, derived from the deployment URL's host).
- The compiled registry maps a host (or apex, case-insensitively) to a Brand: `registered_brand_key`
  (`views/src/brand.rs:1511-1516`) and `brand_key_for_apex` (`views/src/brand.rs:1482`); each Brand's
  `www.` and `staging.` hosts are listed at `views/src/brand.rs:1385-1394` (five Brands).

### Discover the corresponding Brand/Firm

- `resolve_brand` (`cli/src/letterhead.rs:93-118`): trims the declaration, requires a host (`Undeclared`),
  resolves the host through the registry (`UnknownHost` names the host), and refuses a declared brand that
  disagrees with the host-selected Brand (`BrandMismatch { declared, resolved }`). `LetterheadError`
  (`cli/src/letterhead.rs:31-45`) and its `Display` (`letterhead.rs:63-91`) give each refusal an
  actionable, correct-the-input message.
- The Brand's Firm identity supplies presentation and identification: `for_brand`
  (`cli/src/letterhead.rs:121-141`) sets `letterhead.entity` from the Brand's `firm.legal_entity`; runtime
  rows from `/app/admin/brands` flow through `for_brand_runtime` (`letterhead.rs:178-221`) and
  `runtime_frame` (`letterhead.rs:246-260`), including `firm_legal_name`. The render path is
  `project_letterhead` (`cli/src/main.rs:8778-8796`) with a bounded-budget store attempt
  (`main.rs:8798-8822`) and an announced compiled fallback.
- The questionnaire binds `firm.legal_name` from the same resolution: `project_firm_legal_name`
  (`cli/src/main.rs:8764-8779`) and `fill_firm_legal_name` (`main.rs:8753-8761`) — but see §5 for its soft
  failure mode.
- CLI-side Firm discovery by name + brand key: `remote::resolve_firm` (`cli/src/remote.rs:4435-4492`) lists
  only Firms visible to the caller, refuses ambiguity, and — per LAW-255 — names which case refused a brand
  key: worn by a different Firm (naming the wearer), or unknown/outside the directory
  (`remote.rs:4469-4492`).

### Pass validation only when manifest and live identity agree

- `Manifest::check_live_ownership` (`cli/src/projects/manifest.rs:188-206`): an absent pair is allowed (it
  can be filled from live); a present pair must equal the live row's `firm_id` + `brand`, else
  "navigator.yaml Firm and brand differ from the live Project".
- Enforced from three callers:
  - before the local gate may rewrite checkout files — `check_ownership_before_fixes`
    (`cli/src/projects/gate.rs:45-71`, dispatched by `ownership_preflight` at `gate.rs:29-42`);
  - in the live gate status — `check_live_with_token` (`gate.rs:66`) and `check_live_handles`
    (`gate.rs:362`), the latter refusing before any handle comparison;
  - in `project set --from-live` — `manifest_ownership` (`cli/src/projects/set.rs:192-219`, check at
    `set.rs:207`).
- Server side, open: `selected_project_brand` (`portal/src/api.rs:3181-3209`) refuses a one-sided pair,
  requires project-Firm administration, and refuses when the brand's wearer is not the named Firm
  (`portal/src/api.rs:3192-3200`); the store re-resolves and re-checks at insert time inside the
  transaction, with a `THROW 'project_firm_brand_changed'` guard against a concurrent reassignment
  (`store/src/projects.rs:3763-3766`), after `resolve_open_firm` (`store/src/projects.rs:3690-3706`).
- Server side, update: `validate_firm_brand_update` (`store/src/projects.rs:3131-3164`) refuses an unknown
  key, a key worn by no Firm, and a key worn by a different Firm — the LAW-255 naming — and both-fields
  otherwise (`store/src/projects.rs:3157-3163`).
- Manifest shape: Y016 requires `project.firm` to be a UUID and `project.brand` a non-empty key, declared
  together (`cli/src/projects/manifest.rs:623-630`).

## 2. Do `project create` and `project scaffold` take the brand?

Yes — both, by flag; `project set` is no longer the only path.

- `project create --firm <name> --brand <key>` (`cli/src/main.rs:1327-1338`: each flag `requires` the
  other), handler at `main.rs:6336-6353` → `remote::projects_create_named`
  (`cli/src/remote.rs:4239-4332`), which resolves the pair before any write (`remote.rs:4275-4278`) and
  puts `firm_id`/`brand` in the open payload (`remote.rs:4330-4332`).
- `project scaffold --firm <name> --brand <key>` (`cli/src/main.rs:1367-1373`, each requires the other,
  both conflict with `--resume`), passed through `main.rs:6374-6395` into `ScaffoldOptions`
  (`cli/src/projects/scaffold.rs:104-112`) and on to `projects_create_named` (`scaffold.rs:237-248`).
  After the open, `write_ownership` (`scaffold.rs:286-305`) reads the opened row's actual `firm_id`/`brand`
  and writes them into `navigator.yaml` via `set::replace_project_key` — on the fresh path (`scaffold.rs:269`,
  before repository provisioning, so the first commit carries them) and on `--resume`.
- `project set --firm --brand` remains, for adoption/reassignment (`set.rs:94-99`; live-row patch via
  `remote::set_project_firm_brand`, `remote.rs:4406-4427`).
- Caveat — the interactive prompt does not ask: `collect` (`scaffold.rs:45-77`) prompts name, code, client
  name/email, entity/jurisdiction, attestation — never Firm/Brand. Omitting the flags is not an error: the
  server defaults the brand to the deployment default (`portal/src/api.rs:3208`,
  `brand.unwrap_or_else(|| views::brand::brand_key()...)`) and `open_matter` resolves the Firm from the
  brand's wearer (`store/src/projects.rs:3690-3706`), so a fresh interactive scaffold silently lands on the
  default Brand. §7 flags this as a user-owned product decision.

## 3. Is the actionable-refusal posture complete for all four failure classes, including scaffold-time?

Three of four are complete end to end. Unknown-host refusal at scaffold time is the gap. The table names
each refusal's site; the prose under it carries the detail.

| Failure class | Refusal site | At scaffold time? |
|---|---|---|
| Unknown host | Render + live gate/CI preflight | No |
| Wrong Firm | CLI, open, update, live gate | Yes |
| Contradictory Brand | Render, live gate, Y016 shape | Partially |
| Partial setup | Open, update, scaffold resume | Late but actionable |

- **Unknown host** — refused at render (`LetterheadError::UnknownHost`, `letterhead.rs:38-40`, raised in
  `resolve_brand`, `letterhead.rs:106-108`) and by the live gate/CI DNS preflight, whose test names
  `project.host ... does not resolve` (`cli/src/remote.rs:6432-6463`). **Not at scaffold time:**
  `write_tree` writes the deployment host unvalidated (`scaffold.rs:306-324`); nothing checks it against
  the Brand registry. The first refusal is the first letter render or the live gate.
- **Wrong Firm** — refused at the CLI (`resolve_firm` names the wearer or the unknown key,
  `remote.rs:4469-4492`), at open (wearer must equal the named Firm, `portal/src/api.rs:3192-3200`), at
  update (`validate_firm_brand_update`, `store/src/projects.rs:3153-3161`), and at the live gate
  (manifest-vs-row mismatch, `gate.rs:66`, `gate.rs:362`). Scaffold-time: yes — the pair is resolved
  before the matter opens (`remote.rs:4275-4278`).
- **Contradictory Brand** — refused at render (`BrandMismatch`, `letterhead.rs:45-51`, raised
  `letterhead.rs:111-117`) and live (`check_live_ownership`, `manifest.rs:188-206`, from all three
  gate/set callers), with the Y016 pair shape rule (`manifest.rs:623-630`). Scaffold-time: partially —
  the written pair is copied from the live row (`scaffold.rs:293-300`), so a fresh scaffold cannot
  contradict; but the offline content gate does not cross-check `project.brand` against the host's
  compiled Brand (§6, item 4).
- **Partial setup** — open refuses a brand no live row carries (`store/src/projects.rs:1297-1300`,
  `UnknownBrand`) or no Firm wears (`:3692-3698`, `BRAND_WORN_BY_NO_FIRM`), and the row then always
  carries both coordinates; update refuses a one-sided patch (`store/src/projects.rs:3157-3163`);
  scaffold resume over a firmless adopted row refuses "opened Project has no owning Firm"
  (`scaffold.rs:293-296`). That refusal is late — after the tree is written and the matter open —
  but recoverable via `--resume`.

An absent pair in an existing manifest is a deliberate valid resting state (`manifest.rs:190-193`, "an
absent pair can be filled from live"), fillable with `project set --from-live` (`set.rs:192-219`).

## 4. No-retrospective-restyling invariant

No violation found. The render path's own contract states it: a template re-render reflects the letterhead
its Brand currently publishes "while executed and filed artifacts keep their stored bytes (ENG-1148)"
(`cli/src/letterhead.rs:25-28`; repeated at `cli/src/main.rs:8786-8788`). Concretely:

- Renders produce new bytes from templates; executed/filed documents are the stored uploads
  (`site document upload` files bytes as-is). No code path re-renders an uploaded artifact under a new
  Brand — the only letterhead consumers are render-time (`project_letterhead`) and questionnaire-fill
  (`fill_firm_legal_name`).
- The Y027 autofix is confined to working-tree drafts and skill/readme prose, is a warning, and routes
  signature-block/From-line identification through `{{firm.legal_name}}`, which render fills from the
  Brand declared in `navigator.yaml` (`cli/src/projects/repository.rs:2677-2685`) — so the legal name
  survives where identification is required. Its letter-draft rewrite risk is the separate LAW-300 follow-up
  (`docs/research/autofix-firm-name-research.md` on its task branch), not a LAW-254 violation.

## 5. Test surface for the four failure classes

Existing coverage, by class:

- Unknown host: `a_declared_brand_requires_a_known_host` (`letterhead.rs:271-279`),
  `every_registered_host_and_apex_resolves_to_its_brand` (`letterhead.rs:291-306`), refusal-message test
  (`letterhead.rs:308-317`); DNS preflight: `remote.rs:6432-6463`.
- Wrong Firm: `firm_resolution_checks_brand_and_authorized_directory` (`remote.rs:5636+`) and
  `firm_resolution_names_why_a_brand_is_refused` (LAW-255, `remote.rs:5677-5701`); live-gate mismatch:
  `a_manifest_ownership_mismatch_refuses_before_reading_handles` (`cli/src/projects/gate.rs:875-893`, also
  asserting the check fails fast on one request); store update:
  `store/tests/project_update_is_a_patch.rs:276-337` (pair required together, pair written, worn-by-another
  refused by name, unknown key named as unknown).
- Contradictory Brand: `a_declared_brand_cannot_override_the_host` (`letterhead.rs:262-269`) and
  `a_matching_declaration_asserts_the_hosts_brand` (`letterhead.rs:281-289`).
- Partial setup: open on an unworn brand refuses and writes no row
  (`store/tests/matter_open_writes.rs:209-224`); open on a worn brand resolves the Firm
  (`matter_open_writes.rs:185-203`); pair shape tests in `manifest.rs:1412-1437`.

Gaps:

1. **`scaffold::write_ownership` has no test.** Every scaffold test passes `firm: None, brand: None`
   (`scaffold.rs:445-451`, `623-629`, `647-653`, `669-675`); the suite covers prompts and directory
   validation only (`scaffold.rs:377-673`). Nothing asserts that a fresh scaffold with `--firm/--brand`
   writes the live pair into `navigator.yaml`, or that a firmless adopted row refuses.
2. **The offline content gate has no brand-vs-host cross-check to test** — the check does not exist
   (§6, item 4).
3. The portal's `selected_project_brand` has no direct unit test; its logic is thin over
   `store::firms::firm_id_for_brand_key`, which the store tests exercise, so this is low priority.

## 6. Verdict and remainder

**The issue is effectively Done at the level the governing decision defines.** New and adopted Projects can
declare a registered Brand host, the corresponding Brand/Firm is discovered through the registry and the
live row, validation passes only when manifest and live identity agree, all four failure classes refuse
with named causes at the layer that can act, and nothing restyles an executed artifact. Like LAW-299, the
honest disposition is "shipped; close or close-with-follow-ups." The remainder is real but small:

1. **Scaffold-time host check (recommended, small).** `write_tree`/`write_ownership` could refuse — or
   loudly annotate — a deployment host the compiled registry does not know, instead of deferring the
   refusal to the first letter render. Trade-off: dev deployments (KIND, ephemeral hosts) legitimately do
   not resolve to a compiled Brand, so a hard refusal needs an explicit override or a warning-only form;
   that choice is exactly why I would not fold it in silently.
2. **`write_ownership` test (recommended, cheap).** One tokio test against a mock server: flags produce the
   live pair in `navigator.yaml`; a firmless row refuses. Extends the existing scaffold test harness.
3. **Offline brand-vs-host cross-check (optional).** `manifest::lint` could consult the compiled registry
   (the `cli` crate already depends on `views`) and warn on a contradiction, catching it before render.
   Trade-off: duplicates `resolve_brand`'s job in a second surface; only worth it if render-time refusal is
   judged too late for CI contexts that never render.
4. **Questionnaire Firm-name soft failure (flag, small).** `project_firm_legal_name` returns `None` on any
   resolution failure (`cli/src/main.rs:8771-8773`, `.ok()?`), so a Project with an unresolvable
   declaration silently loses its Firm legal name in the questionnaire fill while the letter render refuses.
   A stderr warning in the Project-resident case would match the refusal posture; the silent form is
   correct for templates outside any Project, so the fix must distinguish the two.

Items 1 and 4 involve product posture, not just mechanics — see §7.

## 7. User-owned decisions (flagged, not decided)

1. **Should interactive scaffold ask for Firm/Brand?** Today the flags are opt-in and omission silently
   selects the deployment default Brand (`portal/src/api.rs:3208`). That is either a convenient default or
   a silent identity selection, depending on the reading of "actionable refusal" — the governing decision
   does not say whether a *silent default* counts as a partial-setup state. Nick should decide whether
   `collect` gains a prompt (and whether defaulting is documented or refused).
2. **Scaffold-time unknown-host: refuse or warn?** Priced in §6 item 1; the refusal/warning fork turns on
   how much dev-host friction is acceptable.
3. **Disposition of LAW-254 itself:** close as shipped (my reading — the acceptance items and the identity
   decision are met and tested), optionally filing items §6.1-§6.4 as follow-ups, with §6.1+§6.2 as the
   only pair I would actually batch into one small PR.
