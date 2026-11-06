# Letterhead (LAW-266) and Attorney Signature (LAW-280) — code findings

Research on `neon-law/navigator` at `origin/main` commit `02b251b`
(feat(projects): link a Project to an existing Drive folder (#463)), branch
`task/research-navigator-letterhead-brand-data-MQMYNMtq`. Read-only; every
path below was read this session.

**Load-bearing assumption (inherited from ENG-1148):** the letterhead a letter
wears must be the identity its Brand publishes — `project.host` → Brand →
owning Firm, host authoritative, `project.brand` an assertion only. Evidence
that confirms it in code: the refusal tests in `cli/tests/render.rs`
(`a_declared_project_brand_cannot_override_the_host`,
`a_host_with_no_brand_is_refused_rather_than_framed_as_the_default`). What
would break it: a decision that offline CLI renders should consume runtime DB
rows, which trades re-render determinism for admin-editable letterheads
(discussed in the recommendation).

---

## 1. `cli/src/letterhead.rs` as it stands (209 lines)

Resolution flow, in two functions:

- **`resolve_brand(brand: Option<&str>, host: Option<&str>) -> Result<BrandKey, LetterheadError>`**
  1. Trim; empty/whitespace counts as absent.
  2. Optional `project.brand` is parsed with `BrandKey::parse`; unknown key →
     `LetterheadError::UnknownBrand(key)`.
  3. `project.host` is **required** → `LetterheadError::Undeclared` otherwise.
  4. Host resolves through `views::brand::registered_brand_key(host)` (exact
     registered host, case-folded) with a fallback to
     `brand_key_for_apex(host)` (the Brand's apex domain). No hit →
     `UnknownHost(host)`.
  5. A declared `project.brand` that disagrees with the host-selected key →
     `BrandMismatch { declared, resolved }`. The declaration can never win.

- **`for_brand(key: BrandKey) -> Result<pdf::Letterhead, LetterheadError>`**
  builds the frame entirely from the **compiled registry**:
  - `key.resolve_branding(&views::brand::DEFAULT_BRANDING)` → the static
    `Branding` for that key;
  - `pdf::Letterhead::from_brand(branding.firm.site_name,
    key.canonical_host(), branding.firm_phone, "")`;
  - `letterhead.entity = branding.firm.legal_entity`;
  - `letterhead.logo = (key == BrandKey::Neon)` — **only the default Brand
    draws the compiled mark**, everyone else opens on a wordmark.

`for_project(brand, host)` is the composition of the two.

**BrandKey source.** `views/src/brand.rs` — a closed 15-key enum (`Neon`,
`DeleteYourData`, `DeleteYourDebt`, `Vesta`, `Misericordia`, `Abhaya`,
`Summons`, `Daybridge`, `DeathAndDivorce`, `CyberInjuryLaw`, `Aegis`,
`Groundwork`, `GuiebShook`, `LexTecnica`, `IepDefenders`), each with compiled
`hosts()`, optional `apex()`, and a static `Branding` (site name, legal
entity, `firm_phone`, `firm_email`, `primary_domain`, palette, typeface).
Adding a Brand is a code change to this registry.

**Presentation fields available today.** `pdf::Letterhead`
(`pdf/src/format.rs:48`) carries exactly: `name` (wordmark, printed in
letterspaced capitals), `phone`, `email` (always `contact@` the host's
registrable domain — `registrable_domain()` is a deliberately naive
last-two-labels rule), `web` (the host), `entity` (Firm legal name, printed
small beneath the wordmark), `logo: bool`. There is **no postal address
field** and **no font field** on the render struct, and the email is derived,
not stored.

**Where compiled vs runtime data diverge.** The renderer never reads the
runtime store. `store/src/brand_letterheads.rs` exists (see §3) with a
resolved `BrandLetterhead` that carries *more* than the compiled path:
postal address, per-Brand mark choice (wordmark vs uploaded logo), admin-set
email and site URL, and a font family + font object key. None of that reaches
a PDF or Word render today. The two sources also disagree structurally: the
compiled letterhead derives the mailbox from the host, the runtime one from
the Brand's approved `website_url`.

Error presentation is already house-shaped: every `LetterheadError` variant
renders an actionable sentence naming the offending key or host and what to
correct, and refuses — never falls back to a default letterhead.

## 2. PDF and Word render paths that consume letterhead

**CLI (the only letterhead-consuming render path):**

- `cli/src/main.rs` `project_letterhead(file, format)` (≈8636): only
  `pdf::OutputFormat::Letter` gets a letterhead; anything else, or a template
  outside a Project, gets `pdf::Letterhead::default()`. It finds the nearest
  `navigator.yaml` (`projects::matter::find_manifest`), parses it, and calls
  `cli::letterhead::for_project(manifest.brand, manifest.host)`. A refusal
  propagates as a string error and no artifact is written.
- `render_notation_artifact` (≈8750) fans the single `pdf::Letterhead` into
  both engines:
  - **PDF:** `pdf::render_document_with_options(body, format, &letterhead, …)`.
  - **Word:** `word::RenderLetterhead { name, entity, phone, email, web,
    logo_png: if letterhead.logo { pdf::firm_logo_png() } else { &[] } }` →
    `word::render_notation(...)`. In `word/src/render.rs` this becomes the
    first-page header (`word/header2.xml`, `<w:titlePg/>`): wordmark in
    capitals, entity line, contact line `phone · email · web`, and the
    compiled PNG mark when non-empty.

A deliberate documented invariant (`cli/src/main.rs` ≈8641): the letterhead is
built from the **compiled registry — source, not whatever brand bundle is
mounted** — "so a re-render of a sent letter prints the same letterhead."
Any LAW-266 work that moves rendering onto runtime rows must reconcile with
this sentence.

**Server-side render in this repo:** yes, but it does not consume
`cli::letterhead`. `portal/src/retainer_walk.rs` renders letter-framed bodies
with `pdf::Letterhead::default()` on purpose (comment ≈3006: the runtime
letterhead reaches object storage "instead of falling back" only via
`cli::run_render`, which is the CLI path), and `portal/src/api.rs` (≈3196)
renders the pitch-offboarding letter from Typst with no letterhead struct.
So there is no server preview of a Brand's letterhead today; if LAW-266 wants
one, retainer-walk is the existing seam.

## 3. Brand data model: runtime store, uploads, admin surfaces

**A runtime Brand store already exists in this repo — the gap is the renderer
seam, not a missing store.**

- **`store/src/brands.rs`** — `Brand` rows (SurrealDB; `store/src/postgres/`
  has no brands module): `id, name, key, firm_id, primary_color, accent_color,
  typeface, is_law_firm, legal_entity, logo_object_key, logo_content_type,
  font_family, font_object_key, font_licence, website_url`. `set_logo` /
  `set_font` attach **uploads as public-bucket object keys**
  (`fonts/brands/<key>/<hash>.woff2`), storing object key + content type, and
  the font select is limited to the Firm's own uploads (ENG-659). There is
  also a compiled typeface/palette catalog (`views/src/brand_presentation.rs`
  `TYPEFACES`, `PALETTE`) — a `typeface` naming a catalog face is not an
  upload (the letterhead store's tests assert this).
- **`store/src/brand_letterheads.rs`** (629 lines, LAW-266's data layer,
  already landed): one `brand_letterhead` **PostgreSQL** row per Brand (no
  Surreal source), keyed by Brand id. `LetterheadFields` holds only what an
  admin set: `wordmark`, `mark` (`wordmark | logo`), `phone`, `email`,
  `site_url`, `postal_address`, `font_family`. `resolve(brand, stored)`
  assembles `BrandLetterhead` with clean fallbacks — unset wordmark → the
  Brand's name; unset site → the Brand's approved website; unset email →
  `contact@` that site's registrable domain; Firm legal name always the
  Brand's `legal_entity`. `for_brand_key(surreal, pg, key)` is documented as
  "the one read every letterhead renderer uses" — **no renderer calls it
  yet**; its only consumers are the admin edit pages and their tests.
- **Authorization:** `save_for_actor` gates writes to the Brand's Owner or the
  Admin DRI of the Brand's own Firm (via `store::brands::find_by_key_for_actor`);
  anything else gets a non-disclosing `NotFound`.
- **Admin edit surfaces (in this repo, not external):**
  `portal/src/brand_edit.rs` — the Axum form that maps a
  `BrandLetterheadForm` to `LetterheadFields` and calls `save_for_actor`;
  `webapp/src/brands_edit.rs` — the Dioxus `/app/admin/brands` edit view
  (logo/font read-only there; the form edits colour) with letterhead
  preview/defaults built on `store::brand_letterheads::resolve`. The public
  site that serves the brands is this same repo's `views`/`portal` stack.
  **Nothing brand-related lives outside this repository.**

So LAW-266's remaining open question is exactly the one the brief names:
`cli/src/letterhead.rs` renders from the compiled registry and only the
default Brand's compiled mark, while the editable per-Brand data
(wordmark/logo choice, phone, email, site, address, font) sits in
`brand_letterhead` rows nothing renders from.

## 4. Email drafting surface for LAW-280

**Commands.** `navigator mail` (`cli/src/mail.rs`, dispatched from
`cli/src/main.rs` `Mail`): `Search`, `Read`, `Draft`, `Login`, `Setup`,
`File`, `Parse` (plus filing a saved `.eml` through the matter inbox).
Drafting is `mail draft` → Gmail API `POST/PUT /gmail/v1/users/me/drafts`;
the RFC 5322 raw message is assembled by `draft_raw()` (≈738): To/Cc/Bcc via
`address_list`, header-safe subject (RFC 2047 `encoded_word` when
non-ASCII), `In-Reply-To`/`References` for `--thread` replies, then either a
`text/plain` body or `multipart/mixed` with base64 attachments (Gmail 25 MB
limit checked before any network call).

**Where a signature would be inserted.** `draft_raw()`'s body assembly — the
point where the body is normalized to CRLF text — is the single seam every
draft passes through, and it already has project context available
(`current_project_context()` reads the nearest `navigator.yaml`). Today the
only signature logic is a **stripping** hook, not an insertion one:
`without_signature(body, configured_signature)` removes an exactly-matching
`NAVIGATOR_GMAIL_SIGNATURE` env-var signature block from a replied-to body so
quoting doesn't double-sign (unit tests at ≈2670). There is **no signature
insertion, no per-attorney or per-Brand signature configuration, and no
confidentiality-notice configuration anywhere in the mail path** — those are
greenfield for LAW-280.

**How drafts stay unsent.** By construction, three ways, each testable:
1. There is no `Send` operation in `MailAction` — the enum ends at draft
   update; the doc comment says "The draft stays unsent."
2. OAuth scopes requested at `mail login` are exactly
   `gmail.readonly` + `gmail.compose` — no `gmail.send`.
3. Drafts are created/updated through the Gmail drafts endpoints only.

**Related but distinct surface:** `navigator inbox` (`cli/src/inbox.rs`) is
lawyer-mailbox triage (`List`/`Process`/`ImportFi`/import), not a drafting
surface. Note: the shared sandbox currently shows in-flight inbox work on a
separate branch (`feat/inbox-person-association`, modified
`store/src/inbox.rs`) — LAW-280 spec work should not assume that state.

## 5. Gate path `POST /auth/ci/document-token` and the DNS diagnostic

- **Server:** `portal/src/ci_auth.rs` — `routes()` registers
  `POST /auth/ci/document-token` → `mint_document_token` → `mint_inner`:
  verifies the runner's OIDC JWT (GitHub, plus a parallel Forgejo door at
  `/auth/ci/forgejo/document-token`), binds the run to the live Project whose
  `repository_url` matches, and returns an HMAC-signed `SessionData`
  scoped to metadata reads only.
- **Client:** `cli/src/projects/gate.rs` `check_live()` →
  `remote::resolve_ci_document(host)` (`cli/src/remote.rs` ≈616):
  `base = credentials::base_url(host)`, fetch the runner's OIDC ID token
  audienced to `base`, then `mint_ci_token(client, base, document_mint_path, token)`
  POSTs to `/auth/ci/document-token`.
- **Where the DNS error surfaces:** inside `mint_ci_token`'s `.send()` →
  `unreachable_host_error(base, mint_path, error)` (remote.rs ≈716).
  `is_dns_failure()` walks the reqwest error's source chain for resolver
  phrases ("dns error", "failed to lookup address", "name or service not
  known", …). A DNS failure produces:
  > `project.host `{host}` does not resolve: no DNS record answers for it, so
  > POST {mint_path} never reached Navigator. Publish the host's DNS record,
  > or set project.host to a host this deployment serves.`

**Assessment for the spec:** the actionable "unresolvable `project.host`"
diagnostic largely **already exists** — it names the host, the cause, and
both remedies. What a LAW-266 diagnostics slice could still add: (a) the same
classification for the later `GET /app/api/projects` transport failure in
`check_live_with_token`, which today surfaces as a bare context line;
(b) a pre-flight DNS check with the same message so the failure surfaces
before the OIDC audience request; (c) surfacing the refusal annotation
(`mint_refusal_annotation`, GitHub `::error::` workflow command) for the DNS
case, which currently rides the anyhow context rather than the annotation
path used for non-2xx mints.

## 6. Test conventions to match

- **`cli/tests/render.rs`** (1478 lines): integration tests invoke the **real
  binary** (`assert_cmd::cargo::cargo_bin`) against fixtures in a
  `tempfile::TempDir` — a `navigator.yaml` plus a `VALID` letter template
  (full frontmatter: kind/title/respondent_type/confidential/questionnaire/
  workflow). Refusal tests assert three things: non-zero exit, stderr naming
  the offending key/host, and **no output file written**. Positive Word tests
  extract `word/header2.xml` from the docx and assert brand strings.
  Brands are chosen from `views::brand::BrandKey::ALL` ("so the test follows
  the registry wherever it moves"), never hardcoded names.
- **`cli/src/letterhead.rs` `mod tests`**: invariant-named snake_case tests
  (`a_declared_brand_cannot_override_the_host`), exhaustive iteration over
  `BrandKey::ALL` and every registered host, `assert_eq` on `Result` values,
  and error-message content assertions.
- **`cli/tests/letterhead_brand_parity.rs`**: grounding tests that pin
  `pdf::Letterhead::default()` to `DEFAULT_BRANDING` and check per-host parity
  (name, entity, phone, web, derived `contact@` domain, logo flag) for every
  registered host — the drift alarm between compiled registry and render
  defaults.
- Gate tests (`cli/src/projects/gate.rs`): `wiremock` `MockServer` +
  `#[tokio::test(flavor = "current_thread")]` behind an `ENV_LOCK` mutex when
  env vars are involved.

Acceptance tests for either LAW should follow these shapes: real-binary
end-to-end for render behavior, registry-derived brand selection, refusal
triple (exit/stderr/no artifact), and parity grounding tests wherever two
sources must not drift.

---

## Recommendation

**LAW-266 slice implementable in this repo alone:** re-point the renderers at
the runtime letterhead read that already exists. Concretely:
`cli/src/letterhead.rs` gains a store-backed variant of `for_brand` that calls
`store::brand_letterheads::for_brand_key` (its documented purpose) and maps
`BrandLetterhead` → `pdf::Letterhead`, extending `pdf::Letterhead` and
`word::RenderLetterhead` with `postal_address` and `font_family` (both already
resolved in `BrandLetterhead`), and drawing a non-default Brand's mark from
`Brand.logo_object_key` instead of the hardcoded `key == Neon` rule. The
admin write path, uploads, and authorization are all shipped; the parity tests
in `cli/tests/letterhead_brand_parity.rs` extend naturally to assert
render-vs-row agreement. Everything needed is in this repository.

**The one genuine design decision the spec must make first:** the current
compiled-source choice is documented as re-render determinism ("a re-render of
a sent letter prints the same letterhead"). Runtime rows break that unless the
spec picks a stability story — e.g. an admin edit bumps a revision the CLI
warns about on re-render, or sent letters pin the letterhead revision. Also
note the CLI renders offline by design, so a store-backed render needs a
reachable deployment (as `project gate --check` already requires) with an
explicit compiled-registry fallback for offline use.

**What LAW-280 needs (beyond this repo's current code):** a source of truth
for the signature itself — the natural home is the Brand/Firm letterhead data
already in `store` (a per-attorney signature row referenced from the Firm, or
a per-Brand confidentiality notice on `brand_letterhead`), none of which
exists yet; one insertion seam in `draft_raw()`'s body assembly (where project
context is already read); and preservation of the three unsent guarantees
above as invariant tests (no Send op, `gmail.compose`-only scope, drafts
endpoint only). The existing `NAVIGATOR_GMAIL_SIGNATURE` env var should be
retired into whatever configured signature replaces it, since its only job
today is stripping the very block a configured signature would own.
