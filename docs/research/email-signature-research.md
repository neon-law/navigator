# Attorney email signatures (LAW-280) — code findings

Research on `neon-law/navigator` at `origin/main` commit `8fab325` (fix(gate):
pre-flight DNS check, classified projects fetch, DNS-miss annotations
(#480)), which includes the merged letterhead work (PR #472 as `18e17bd`).
Branch `task/research-navigator-email-signatures`. Read-only; every path
below was read this session. The companion research pass
(`docs/research/letterhead-signature-research.md` on branch
`task/research-navigator-letterhead-brand-data-MQMYNMtq`, at `02b251b`)
predates PR #472 and the signature-insertion work; where its findings are now
stale, this doc says so.

**Load-bearing assumption (inherited from LAW-280):** an email signature the
CLI inserts must be the attorney's approved block — identity and
confidentiality notice set by someone authorized, inserted exactly once,
visible to the attorney for review before any send. Evidence that confirms
the review half in code: the draft always stays unsent (three independent
guards, §3), and the insertion is idempotent-plus-refusing rather than
blindly appended (`cli/src/mail/signature.rs`, §1). What would break it: a
decision that signatures are a per-Brand (not per-attorney) setting, or that
Gmail's own account-level signature makes insertion redundant — both
discussed in the recommendation.

---

## 1. The insertion seam as it stands

**`navigator mail draft`** (`cli/src/mail.rs`): the `Draft` arm of
`run_inner` (≈1666) builds a `DraftRequest` whose `configured_signature`
field is read at call time from the environment —
`std::env::var("NAVIGATOR_GMAIL_SIGNATURE").ok()` (≈1684) — then calls
`save_draft_command` (≈1784). That function is the whole composition, in
order:

1. Read the body from `--body-file`; refuse an empty body (before any
   network call).
2. **Insert the signature:** `let body =
   signature::with_signature(&body, configured_signature.as_deref())?;`
   (≈1805). This runs *before* `draft_raw`, so the signature is ordinary
   body text by the time MIME assembly happens.
3. Read attachments from disk; refuse the 25 MB Gmail limit early
   (`MAX_MESSAGE_BYTES`, ≈40).
4. Connect to Gmail, fetch the profile, resolve reply recipients/subject
   from `--thread` when given.
5. Assemble the RFC 5322 raw message in `draft_raw(to, cc, bcc, subject,
   body, in_reply_to, attachments)` (≈725): header-safe subject (RFC 2047
   `encoded_word` when non-ASCII), `In-Reply-To`/`References` for replies,
   then either `text/plain; charset=UTF-8` or `multipart/mixed` with base64
   attachments. It does **not** know signatures exist — it renders whatever
   body it is handed. Step 2 is the only seam a configured signature needs;
   nothing in `draft_raw` would change.

**`cli/src/mail/signature.rs`** (124 lines) owns insertion. `with_signature`
returns the body plus one `-- \n`-delimited block, and it already enforces
the review posture:

- **Missing configuration is a refusal, not a skip**: `None`, blank, or a
  bare `--` yields `SignatureError::MissingConfiguration`, whose message
  names `NAVIGATOR_GMAIL_SIGNATURE` — and the CLI refuses before Gmail is
  contacted (pinned by `a_missing_draft_signature_is_refused_before_connecting`,
  ≈4039).
- **Idempotent across repeated updates**: an exactly matching trailing
  signature is replaced in place, so updating a draft never double-signs
  (`configured_signature_is_visible_once_after_repeated_updates`).
- **Ambiguity requires review**: a matching block sitting on its own lines
  *before* other text (quoted history, or a second signature the operator
  pasted) returns `SignatureError::AmbiguousExistingBlock`; the command
  refuses before contacting Gmail and leaves any existing draft untouched
  (`an_embedded_signature_requires_review_before_updating_a_draft`, ≈4005).
  Quoted lines prefixed with `>` never trigger the refusal, and inline prose
  that merely contains the same words is preserved
  (`signature_cleanup_preserves_prose_and_quoted_context`).
- Line endings are normalized (CRLF/CR in both body and configured value).
- The refusal messages never leak the configured signature's content — tests
  pin that neither the env-var name twice nor the signature text appears in
  the embedded-block error (≈4025–4031).

**Stale since the LAW-266 research:** that doc described the only signature
logic as a *stripping* hook (`without_signature`) and said insertion was
greenfield. Current `main` has full insertion with dedup and the refusal
semantics above, plus `DRAFT_HELP` (≈50) and `docs/mail-cli.md` §"Signature"
documenting the behavior. The env-var source is still the only source.

## 2. Signature sources today: none in the store

- **`store/src/brands.rs`** — `Brand` (≈156) carries `name`, `key`,
  `firm_id`, colors, typeface, `is_law_firm`, `legal_entity`, logo/font
  object keys, `website_url`. No email, no signature, no notice fields.
- **`store/src/firms.rs`** — `Firm` (≈88) carries `name`, `slug`, `status`,
  `entity_id`; `PersonFirmRole` (≈212) carries membership and `is_dri`. No
  email or signature settings.
- **`store/src/brand_letterheads.rs`** — the letterhead row holds
  `wordmark`, `mark`, `phone`, `email`, `site_url`, `postal_address`,
  `font_family`. Its `email` is the brand's public contact mailbox, not an
  attorney's sending identity, and there is no signature or confidentiality
  notice field.
- **`store/src/signatures.rs`** is a **term collision, not a source**: it is
  DocuSign wet-ink e-signature provider plumbing (`SignatureProvider`,
  consumed by `notarizations.rs` and `signature_webhook_journal`). LAW-280
  work that greps for "signature" must not touch or reuse these names.
- **`store/src/persons.rs`** stores one `email` (unique via `email_lower`)
  per person — an identity login field, not a signature block.

So the only configured signature anywhere in the mail path is the
`NAVIGATOR_GMAIL_SIGNATURE` environment variable, set privately per lawyer
machine. Per-attorney, per-Firm, and per-Brand signature configuration are
all greenfield, as the LAW-266 research concluded — and that conclusion
still holds at `8fab325`.

## 3. The draft-sending posture: what a signature can and cannot claim

Three independent guards keep drafts unsent, each test-pinned:

1. **No send operation.** The `Operation` enum (≈211) is
   `SearchThreads | ReadThread | ReadMessage | ReadProfile | CreateDraft |
   UpdateDraft | ModifyMessage` — `endpoint()` (≈230) maps only those, and
   `gmail_endpoints_expose_reads_draft_writes_and_archive_only` (≈2365) pins
   the full list. `ModifyMessage` is the `inbox process --apply` archive
   label write, with a `modify` path suffix — never trash, never send.
2. **Compose-only scopes, checked both directions.** `GMAIL_SCOPES`
   (`cli/src/mail/login.rs` ≈33) is exactly `gmail.readonly` +
   `gmail.compose`. `check_granted_scopes` (≈96) refuses a grant missing
   either scope or carrying any other — including the full
   `https://mail.google.com/` — and revokes it at Google rather than
   redeeming it. Every later token refresh re-checks.
3. **Drafts endpoints only.** `save_draft` posts/puts
   `/gmail/v1/users/me/drafts`; wiremock-based draft tests additionally
   assert no request path contains `send` (≈2796, ≈3003, ≈3367), and
   `draft_headers_cannot_inject_a_send_path` (≈2658) pins header-safety.
   The module doc (≈1429) states it outright: "navigator mail never sends,
   replies, or forwards: it has no send verb and calls no send endpoint," and
   the setup report must "never claim send access" (≈3657).

**Consequence for LAW-280:** a signature can be *inserted into an unsent
draft the attorney reviews and sends personally* — nothing more. It cannot
claim delivery, and the CLI cannot silently emit signed mail. Note the
distinction `docs/mail-cli.md` (≈151) already draws: Gmail's account-level
`users.settings.sendAs` signature is a web-UI composition setting; this CLI
supplies the complete draft body, so the account signature does not appear in
Navigator drafts. Whatever configured source LAW-280 adds, the existing
insertion-and-review semantics are exactly what the spec wants to preserve.

## 4. Prior art: PR #472's runtime-row read pattern

The letterhead work is the template for a configured signature read at
compose time:

- **One PostgreSQL row per Brand, keyed by Brand id** (`store/src/
  brand_letterheads.rs`, 669 lines): `put_row`/`find_row` over
  `LetterheadFields`; born in PostgreSQL with no Surreal source.
- **The one read every consumer uses:**
  `for_brand_key(surreal, pg, key)` (≈431) resolves the Brand from Surreal
  and assembles `BrandLetterhead` via `resolve(brand, stored)` (≈253) with
  documented fallbacks (unset wordmark → Brand name; unset site → the
  Brand's approved website; unset email → `contact@` the registrable
  domain). A consumer hands it a Brand key and gets the resolved struct —
  the row's `updated_at` rides along as the revision provenance names.
- **The CLI runtime read, with an announced fallback**
  (`cli/src/main.rs` ≈8709–8760): `attempt_runtime_letterhead` wraps a
  bounded (3-second) attempt — `connect_from_env` for both stores,
  `cli::letterhead::for_brand_runtime` → `for_brand_key` — and on any
  failure (connect, read, or budget) falls back to the compiled registry,
  *announced*, never silently. Rendered-from-runtime and compiled-fallback
  sources are both named in the artifact's provenance. Executed and filed
  artifacts keep their stored bytes; only template re-renders reflect
  current settings.
- **The write path and its gate:**
  `save_for_actor(surreal, pg, actor_role, actor_person_id, key, input)`
  (≈400) resolves the Brand through `brands::find_by_key_for_actor`
  (`store/src/brands.rs` ≈644) before anything is written — tenancy-aware
  when `postgres::tenancy` is enabled, otherwise Owner-only for firmless
  Brands and the `FirmCapability::ManageBrand` capability otherwise — then
  `validate()`s and stores. An unauthorized actor gets a non-disclosing
  `LetterheadError::NotFound`.
- **The admin surface:** `portal/src/brand_edit.rs`
  `post_brand_letterhead` (≈183) — `POST /app/admin/brands/{key}/letterhead`
  — maps a `BrandLetterheadForm` to `LetterheadFields` and calls
  `save_for_actor`, redirecting back with a saved/failed banner.

**The gap the mail path would have to cross:** `save_draft_command` runs on
the lawyer's machine with only a Gmail OAuth token. It never constructs
store connections — unlike the render path, which reaches the deployment's
SurrealDB and PostgreSQL through `connect_from_env` exactly as
`attempt_runtime_letterhead` does (and the gate already requires a reachable
deployment for renders). A configured signature read at compose time has two
candidate shapes: the same direct store read the render path uses (requiring
the lawyer machine's store credentials on the draft command), or a
portal API route backed by `for_brand_key`-style resolution (no store
credentials on the machine). That choice is the spec's, but the
runtime-fallback-announced pattern and the resolve/save split are proven
here either way.

## 5. Risks for the spec

- **Who may edit a signature.** The letterhead gate resolves the *Brand*
  through `find_by_key_for_actor` and the `ManageBrand` firm capability. A
  signature is per-attorney in the env-var design; a per-attorney row's
  subject is a `Person`, not a Brand, so that gate does not transfer
  unchanged. Whoever may set an attorney's signature can sign mail as that
  attorney — the spec should decide whether the attorney themself, their
  Firm's Admin DRI, or an Owner holds that authority, and it should reuse
  the non-disclosing-`NotFound` refusal shape rather than leaking which
  identities exist.
- **Impersonation.** The current design has a virtue worth keeping: the
  signature reaches the draft through a private env var the lawyer sets, so
  no other person can alter what signs their mail. A centralized row
  replaces that with an edit-authority question (above) and makes the block
  visible to whoever can read the store — consistent with how letterhead
  contact details already behave, but new for attorney identity.
- **Term collision.** `store/src/signatures.rs` already means DocuSign
  e-signature. A new module should be named for what it is (e.g.
  `mail_signatures`) so greps for "signature" do not conflate the two.
- **Scope boundary worth keeping pinned.** Any LAW-280 slice must leave the
  three unsent guards (§3) intact — the endpoint allow-list test, the
  two-scope check, and the no-`send`-path assertions — since
  `gmail.compose` can send at Google's level and the endpoint list is the
  only guard. An invariant test for "the draft body with a configured
  signature still travels the drafts endpoints" would extend the pattern.
- **Env-var migration.** `NAVIGATOR_GMAIL_SIGNATURE` is documented in
  `DRAFT_HELP` and `docs/mail-cli.md` and pinned by tests naming it. The
  LAW-266 research's closing point still stands: its only job is to supply
  (and dedup) the very block a configured signature would own, so the spec
  should retire it into the replacement rather than run two sources.

## Recommendation

The seam is smaller than the LAW-266 letterhead problem was: insertion,
dedup, review-refusals, idempotency, and the unsent guarantees are all
shipped and tested at `8fab325`. What LAW-280 adds is a **source of truth**,
and the open design decisions are (1) its subject — per-attorney row versus
a per-Brand/Firm confidentiality notice plus a per-attorney identity block —
(2) its transport to the lawyer's draft command, direct store read versus a
portal route, trading the render path's proven `connect_from_env` pattern
against keeping store credentials off the mail flow; and (3) the edit
authority for a block that signs as a specific attorney, which cannot simply
reuse the Brand's `ManageBrand` gate. The PR #472 pattern — one row per
subject, `resolve()` with documented fallbacks, `save_for_actor` with a
non-disclosing refusal, an announced compiled (here: env-var) fallback, and
parity tests that follow the schema wherever it moves — is the shape to
repeat.
