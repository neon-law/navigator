# DocuSign e-signature — setup, signing flow, and production cutover

How Neon Law Navigator sends a retainer for signature, how a client reaches the signing ceremony from the portal — the
ceremony itself runs on DocuSign's own site, never inside a Navigator page — and how each cloud deployment receives an
isolated DocuSign attachment. The web app resolves credentials from each [Firm connection](#firm-connection). Standalone
sandbox tools read credentials from the local environment or a gitignored `.env.local`.

The signature seam lives under `portal/src/`: [`signature.rs`](../portal/src/signature.rs) (the `SignatureProvider`
trait, the provider selection, and the DocuSign + fake impls), JWT-grant auth in
[`docusign_auth.rs`](../portal/src/docusign_auth.rs), the embedded signing routes in
[`esign_view.rs`](../portal/src/esign_view.rs), and the completion [webhook
source](../portal/src/esignature_webhook.rs).

## Selecting the provider is explicit and fails closed

Which provider a boot runs is one environment value with no default and no fallback: `NAVIGATOR_SIGNATURE_PROVIDER`
accepts exactly `fake` or `docusign` (`portal::signature::SignatureProviderKind::from_env`).

- **Unset or misspelled** — the boot refuses to start, naming the two accepted values. A fresh checkout sets `fake`
  (see `.env.example`); nothing ever runs "with the stub because DocuSign was unset" — a send-for-signature button that
  quietly wrote fake envelopes is the demo-shaped failure this removes.
- **`fake` in a production environment** — the boot refuses to start: the fake provider creates envelopes with no legal
  weight. `NAVIGATOR_ENVIRONMENT` unset *is* the production profile, so the fake only ever boots where a developer named
  dev explicitly.
- **`docusign`** — the real provider, which selects the encrypted [Firm connection](#firm-connection) for
  each Project. Signature operations refuse when that connection cannot be resolved.

`fake` is the local signature loop: envelopes persist to disk, anchors are validated against the prepared PDF's text,
each recipient signs a real local page, and completion is delivered as a DocuSign-Connect-shaped HMAC-signed webhook —
so a fresh checkout proves the whole loop and self-tests without a DocuSign account.

## One attachment per deployment

Today's two-deployment topology (see [`environments.md`](environments.md)) uses two separately revocable attachments:
one demo attachment for staging and one production attachment for the production deployment. An attachment consists of
the DocuSign account, app/integration key, impersonated user, RSA keypair, consent grant, REST and OAuth hosts, Connect
configuration, HMAC key, and Navigator webhook path secret.

DocuSign Go-Live copies an approved integration key into production rather than moving it. That permits an app identity
to exist in both tiers, but it does not make the two tiers one credential boundary. Configuration is not copied
automatically.

— [Docusign Go-Live](https://developers.docusign.com/platform/go-live/)

The safe boundary is:

| May be promoted from demo to production | Unique to every deployment attachment |
| --- | --- |
| integration-key app identity | account, base, OAuth, user, RSA, consent, HMAC, webhook |

The RSA keypair does not carry over. The OAuth host also differs — `account-d.docusign.com` for demo and
`account.docusign.com` for production. Do not copy an entire DocuSign block between deployments, even when two
attachments ultimately use a promoted integration-key GUID. See the complete signup matrix in
[`provider-environment-parity.md`](provider-environment-parity.md).

### Testing never burns the production envelope quota

CI exercises the real DocuSign adapter against a loopback Wiremock server using synthetic credentials. The draft
contract renders a PDF, creates one unsent envelope, and reads its status and recipients twice. The Project API test
uses PostgreSQL to check participation, signer identity, and read-only behavior. Run the HTTP contract locally with:

```bash
cargo test -p server --test docusign_sandbox draft_contract_uses_mock_docusign_http
```

The opt-in live rehearsal uses the same draft check on a free [DocuSign developer
account](https://developers.docusign.com/platform/account/). It requires the demo REST and OAuth endpoints. The live
test refuses to use credentials on GitHub Actions. Production traffic uses its own account and credentials.

If a promoted integration-key GUID exists in demo and production, production remains gated by its unique account, API
user, RSA keypair, consent, and HMAC key. Treat the shared GUID as an identifier, never as proof of shared credentials.

## Which templates are signed (template-agnostic send path)

The send path is keyed off the notation's **template code**, not the retainer. `send_notation_for_signature`
([`portal::signature_flow`](../portal/src/signature_flow.rs)) gates on the rendered PDF being present, resolves the
workflow spec via `workflows::bundled_spec_yaml(code)`, builds the manifest from the template's `signers:` list (default
`[client, firm]`) in declaration order — resolving each signer from the questionnaire answers when present and otherwise
from the notation's bound Person row — and hands the envelope to the provider keyed on the notation id. Adding a signed
template is a template + spec, not a new handler — the spec just needs the retainer's shape: an `intake_persisted__*` →
`lawyer_review` → `generate_pdf__*_pdf` → `sent_for_signature__pending` chain.

Two postures, one envelope: **prepare** holds the envelope as a draft on the provider (DocuSign `status: created`, fake
provider `Prepared`) without advancing the workflow — the lawyer's screen keeps saying review; **send** dispatches and
promotes any prepared draft rather than creating a second envelope. Both validate placeholders, anchors, and tabs.
Before dispatch, Navigator also checks the saved Firm signer and the fixed onboarding email against the expected values.
From the portal REST door, `POST /app/api/notations/:id/signature` with `{"prepare": true}` drafts and with no body (or
`false`) sends. From a Project checkout, `navigator notation envelope prepare <notation-id>` drafts and `navigator
notation envelope send <notation-id>` dispatches, stamping the returned envelope id on the notation's document pointer;
`navigator notation envelope status <notation-id>` reads per-signer state and the provider's signer names and email
addresses.

After preparing a draft, open the sender-view URL printed by the CLI to review the placed fields. Add `--open` to
`navigator notation envelope prepare <notation-id>` to launch the browser. Continue back to Navigator, then run
`navigator notation envelope send <notation-id>` to dispatch.

On the lawyer workbench, **Onboarding retainers** groups your participating Projects by whether a signed retainer is
recorded. Expand a group to see the signing methods, review a Notation, or read a filed agreement. The CLI lists
existing agreements across your participating Projects. Use a Notation ID to prepare a signature-enabled Notation for
review.

```bash
navigator notation onboarding --host "$SITE"
navigator notation envelope prepare "$NOTATION_ID" --host "$SITE" --open
navigator notation envelope status "$NOTATION_ID" --host "$SITE"
```

After the lawyer approves the document, recipients, and fields, send the prepared envelope.

```bash
navigator notation envelope send "$NOTATION_ID" --host "$SITE"
```

Signed templates today:

- **`mutual_nda`** — both parties sign. The shared template takes two ordered People rows for signer names and
  emails, independent of the Notation's bound Project client.
- **`onboarding__letter`** — the firm's engagement agreement; client signs, firm countersigns.
- **`wa__benton_county_declaration`** — the client's own declaration (Benton County Superior Court); the
  client signs, before or after a case number is assigned.

`offboarding__letter` is firm correspondence (firm signature only).

## Prepare a mutual NDA from the CLI

From the Project checkout, with a Navigator session already available for its deployment:

```bash
navigator site notation create mutual_nda --host "$host" --project "$project" \
  --client-email "$project_client_email" --answers-from "$answers_file"
navigator site notation approve --host "$host" "$notation_id"
navigator notation envelope prepare "$notation_id"
```

An existing opening Notation or a filed onboarding document satisfies the engagement-first check. Use an existing
Project client's email for the Notation. Put both signers in the private answers file under `people__signatories`, as
two YAML rows with `name` and `email`, company signer first. Supply each party’s notice contact under `people__notices`
with `name` and `email`. Fill the remaining states in the shared template. If filling stops after creation, fix the file
and resume with `navigator site notation fill`, passing the Notation ID, host, and `--answers-from`. Use the Notation ID
printed by creation for the final commands. After approval, check `navigator site notation status` for `document_ready
true`. Use `navigator site notation document` to download the PDF for review. Preparation creates the unsent provider
draft. The CLI opens the sender view only with `--open`.

## Prepare a pleading for consent signatures

Run `navigator site notation create <draft-code> --client-email <email> --project <code>` from the Project checkout.
Publish the Draft and sync its Matter Caption first. Declare each consenting signer in the Draft's ordered `signers:`
list. Place a Signature Block such as `{{client.signature}}` or `{{prior_counsel.signature}}` at each signing line. Bind
the prior counsel role to its name and email answers. Include a Firm signer when counsel must also sign through
DocuSign. The workflow proceeds from lawyer review through PDF generation to the signature wait state.

Approve the Notation, review its court-paper PDF, then prepare the envelope and review its fields in the sender view.
Use the same Notation ID to send the approved envelope.

## Adopt an envelope sent from DocuSign

Copy the Envelope ID from the envelope details in the Firm's DocuSign account, or copy the "View Completed Document"
link from a DocuSign notification email (its `m=` value is the Envelope ID). Read the envelope before deciding whether
to register it.

```bash
navigator project envelope read "$envelope_id_or_link" --host "$host"
```

The read reaches any envelope in the DocuSign account of a Firm where you are an Admin or Lawyer (`firm_envelopes`),
whether or not a Project records it. It prints the provider's status and each signer's step, and writes nothing. Its
`navigator` block names the Project that records or matches the envelope, when you have firm-side access to it, or a
`reason`: `no_project_match`, `ambiguous_project_match`, `ambiguous_project_hint`, `client_not_signer` (the Project's
client DRI is not a signer), or `project_not_visible`. An ID outside the account answers `envelope_not_found` with
DocuSign's own answer.

For an unregistered envelope, Navigator requires the same unique Project match as automatic completion filing. To
register the envelope, run the following from the Project checkout after publishing the Draft.

```bash
navigator project envelope register "$envelope_id" --host "$host" --project "$project" \
  --draft drafts/onboarding__letter.md
```

The Project's client DRI must be among the signers. Navigator links the envelope to the Draft's Notation and records the
provider's signer status and timestamps on the matter. Refresh known envelopes from the Firm's DocuSign account with
`navigator project envelope status <code> --json --host <host>`. `navigator project status <code>` reads the saved
status. A pending envelope keeps a pitch pending. On completion, Navigator files the signed PDF and Certificate of
Completion with their DocuSign envelope ID and records the provider's completion date. A verified Connect completion
updates the linked envelope, and rerunning the command refreshes it directly from DocuSign.

## Void an envelope

Void an envelope that is out for signature, for example after a pitch is declined. DocuSign emails the reason to every
recipient. A held draft was never shown to anyone, so it moves to DocuSign's recycle bin instead. A completed, declined,
or already voided envelope is refused with `envelope_closed`.

```bash
navigator project envelope void "$envelope_id_or_link" --reason "Engagement declined" --host "$host"
```

Run it only after the lawyer approves voiding that envelope. Refresh a recorded envelope afterwards with `navigator
project envelope status <code> --host <host>` so the matter shows it voided.

## Upload an executed onboarding letter

To file a letter already executed in the Firm's DocuSign account, supply its envelope UUID.

```bash
navigator site document upload --host "$host" --project "$project" \
  --file onboarding_signed.pdf --kind onboarding \
  --signing-method docusign --signed-on 2026-11-17 --envelope-id "$envelope_id"
```

Navigator verifies completion through the Firm's account before filing the document, and writes `docusign_envelope_id`
to its pointer. The [Official](glossary/official.md) rule governs active Project status.

## Authentication: JWT grant

The provider authenticates with **JWT grant** — it signs a short-lived RSA assertion with the firm's integration key and
impersonated user, exchanges it for an access token, and caches that token (re-minting 300 s before expiry). A static
`DOCUSIGN_ACCESS_TOKEN` is kept only as a local/demo fallback. The integration key is the JWT `iss`; the OAuth secret is
*not used* by JWT grant — supplying the secret where the integration key belongs yields `issuer_not_found`.

**Why JWT grant, not Authorization Code / PKCE.** PKCE is an extension of the *interactive* Authorization Code flow: it
needs a human at a browser to log in, uses a one-time `code_verifier`/`code_challenge` (random strings, not the RSA
keys), and still performs a token exchange. For a server that sends envelopes unattended that is strictly worse. JWT
grant is the server-to-server flow built for exactly this case, and it is already minimal: one cached token exchange,
re-minted before expiry, no human in the loop after the one-time consent. There is no DocuSign mode that signs each REST
call with the RSA key directly — every call needs a Bearer token, so the private key's only job is to mint that token.
The integration key + user id + account id are required regardless of grant type: every call is scoped to an account and
user.

The standalone sandbox tools accept the following `DOCUSIGN_*` settings:

- `DOCUSIGN_INTEGRATION_KEY` — the app's Integration Key / OAuth client id (the JWT `iss`). **Not** the OAuth secret.
  `DOCUSIGN_USER_ID` — the impersonated API user GUID (the JWT `sub`); the "API Username", not the email.
  `DOCUSIGN_ACCOUNT_ID` — the API Account ID GUID. `DOCUSIGN_PRIVATE_KEY` — the RSA private-key PEM whose public half is
  registered on the app. `DOCUSIGN_BASE_URL` — the eSignature REST base; `https://demo.docusign.net/restapi` for the
  sandbox. `DOCUSIGN_OAUTH_BASE` — the OAuth host. A sandbox boot may omit it and take the demo default
  `https://account-d.docusign.com`; a production boot may not. Once `DOCUSIGN_BASE_URL` declares the integration,
  `store::deployment::WEB_REQUIREMENTS` demands it, and `portal::config::enforce_deployment_invariants` separately
  rejects both an empty value alongside a JWT integration key and any value naming the demo host. Production is
  `https://account.docusign.com`. `DOCUSIGN_SIGNER_EMAIL` identifies the configured signing lawyer for Projects without
  a lawyer DRI. When a Project has a lawyer DRI, its envelope uses that Person's name and email instead.
  `DOCUSIGN_HMAC_KEY` — the DocuSign Connect HMAC key used to verify completion webhooks (see below).

## Sandbox setup (one-time)

The sandbox/developer account is **permanent and free** — watermarked, non-binding envelopes that consume no allowance.
It is the only environment used for dev, CI, and the `#[ignore]` live test. Navigate (deep-links redirect): sign in at
`https://account-d.docusign.com`, then **Settings (gear) → Integrations → Apps and Keys**. That page shows the User ID,
the API Account ID, the Account Base URI, and your apps with their Integration Keys + RSA keypair management.

1. **Create the app** (`Add App and Integration Key`) → copy the **Integration Key** → `DOCUSIGN_INTEGRATION_KEY`.
2. **Add an RSA keypair** under the app's Authentication. DocuSign shows the private key once — copy it into
`DOCUSIGN_PRIVATE_KEY` (or register your existing public key so the key already in `.env` matches). 3. Add a **Redirect
URI** to the app — needed only to land the one-time consent click. Use a dedicated, app-controlled path,
`https://www.neonlaw.com/docusign/consent-callback`, kept **distinct from** the OIDC `/auth/callback`
([`portal::oauth`](../portal/src/oauth.rs)): JWT grant never sends an auth code back, so this URI is ceremonial and must
not collide with the Google-login callback. `web` serves it as a small "Consent recorded" confirmation page so the
operator lands on a confirmation rather than a 404. 4. From **My Account Information** copy the **API Account ID** →
`DOCUSIGN_ACCOUNT_ID` and the **User ID** GUID → `DOCUSIGN_USER_ID`.

5. **Grant one-time consent** — open this in a browser logged into the sandbox and click **Allow** (substitute the
integration key + a registered redirect):

   ```text
   https://account-d.docusign.com/oauth/auth?response_type=code&scope=signature%20impersonation&client_id=KEY&redirect_uri=REDIRECT
   ```

   JWT grant returns `consent_required` until this is done. Consent is scoped to the **(integration key × impersonated
   user)** pair, so sign in as the *same* user whose GUID is in `DOCUSIGN_USER_ID`. If `consent_required` persists
   *after* a successful Allow, the cause is almost always a user mismatch: one email can have multiple DocuSign
   memberships (e.g. a demo and a production account, or two demo accounts), and consent was recorded for the wrong user
   id. Confirm the **User ID** at the top of Apps and Keys equals `DOCUSIGN_USER_ID`, and that the consent browser is
   logged into the sandbox account that owns the app — not the production account.

## Running the live test (Phase 0 grounding)

The [sandbox tests](../server/tests/docusign_sandbox.rs) use a DocuSign demo account. With demo JWT credentials in the
environment, run the draft lookup to prepare a synthetic PDF and check its envelope ID, status, and signing order twice.
DocuSign keeps the envelope in Drafts.

```bash
cargo test -p server --test docusign_sandbox sandbox_reads_an_unregistered_draft_by_id -- --ignored --nocapture
```

For local runs, put the demo JWT settings in the gitignored `.env.local` or select a staging Google Cloud project whose
Secret Manager holds the same settings. The script compiles the test, loads its credentials, and prints the draft ID.

```bash
script/ceremony-docusign.sh --check-draft
script/ceremony-docusign.sh --check-draft --project-id "$STAGING_PROJECT"
```

On a simulated staging site, configure the sample Project's owning Firm under **Integration secrets → DocuSign
connection**. Navigator encrypts the connection with the deployment's KMS key. Give the Project a client DRI whose email
matches a test signer. Prepare a Notation through the staging site or create a draft on the demo web app, then read its
status through the staging Navigator session.

```bash
navigator project envelope status "$PROJECT_CODE" --envelope-id "$ENVELOPE_ID" --host "$STAGING_HOST"
```

To exercise embedded signing, configure the demo signer email and run the captive-recipient test. It dispatches a demo
envelope and verifies that DocuSign issues a signing URL.

```bash
NAVIGATOR_RUN_LIVE_SANDBOX=1 cargo test -p server --test docusign_sandbox sandbox_accepts_a_retainer_envelope
```

Common errors map directly:

- `issuer_not_found` — wrong integration key (likely the OAuth secret). `consent_required` — redo the consent grant.
  `invalid_grant` / `no_valid_keys` — the private key is not the one registered on the app.
  `USER_DOES_NOT_BELONG_TO_ACCOUNT` — the user / account pair does not match.

What the live run still cannot automate (no API auto-signs an envelope): driving the ceremony to `completed`,
downloading the executed documents, and capturing a real Connect completion/decline payload plus the
`X-DocuSign-Signature-1` header. Those steps ground the webhook + HMAC and must be run by hand against the Connect log.

## Client delivery: captive vs emailed

Each notation carries a `delivery` column (`store/src/schema/navigator.surql`) that selects how the client recipient is
addressed when the single send path builds the signature manifest. Recipients follow the template's `signers:` list in
declaration order. When `firm` is in that set it is a non-captive recipient — it receives the usual emailed link —
regardless of `delivery`.

- **`embedded`** (the default; the standalone retainer walk) — the client is a **captive** recipient: the manifest sets
  `client_user_id` (derived from the notation), so DocuSign suppresses the signing email. Because no email goes out, a
  recipient-view URL is the only door. `GET /app/notations/:id/sign` and `GET /app/lawyer/notations/:id/sign`
  ([`portal::esign_view`](../portal/src/esign_view.rs)) mint one via `SignatureProvider::create_recipient_view`, which
  POSTs `envelopes/{id}/views/recipient` and matches the recipient on the email, userName, and clientUserId triple. It
  **redirects the browser to it**. The ceremony runs on DocuSign's own site; Navigator does not frame it. The URL
  expires in minutes, so it is minted fresh per request. The fake provider returns a deterministic URL in dev/KIND. The
  firm path fits an in-office signing; the client path is the signer's own door.

  The signer may never come back — they close the tab, or finish on their phone — so **nothing depends on the return
  trip**. The completion webhook below is the authoritative path; the redirect's `return_url` only decides where a
  signer who does return lands. The column keeps the name `embedded` because it selects the *captive recipient* model,
  which is what DocuSign calls it, not a rendering choice on our side.

  Both paths are one handler, forking on the session's tier rather than the URL prefix, and both apply two gates —
  **participation says you may look at the matter; identity says you may sign as this person**:

  1. *Participation*, the same fork `portal::documents` applies: `store::access::can_see_project_as_lawyer` for a
     firm-tier session (keeping the Owner/Admin project-scoping bypass documented in
     [`access-model.md`](access-model.md)), `can_see_project_as_client` otherwise.
  2. *Identity*, for a non-firm caller only: the session's person must be the notation's bound signer. A matter can
     carry several client participants, so participation alone would let one of them open another's ceremony. The firm
     lens is exempt because there the caller is never the signer — a lawyer minting the view for the client in the room
     is the in-office case above.

  Both refusals answer `404`, never `403`: a `403` on the identity gate would confirm both that the notation exists and
  that the caller is on the matter. The Rego rule for `/app/notations/**` constrains neither depth nor the third
  segment, so **these handler gates are the entire authorization boundary** for the route;
  `server/tests/esign_redirect.rs` is what holds them.

  The `return_url` handed to the provider is **absolute**, resolved through `portal::openapi::base_url_for` (brand
  `base_url` → `NAV_BASE_URL` → request `Host`). It has to be: a provider redirecting a browser resolves a relative path
  against its own origin, not Navigator's. It is also lens-aware — the firm returns to the matter's step page under
  `/app/lawyer`, a client to their own matter page under `/app/projects`, since the policy refuses a client the
  `/app/lawyer` prefix.

  A notation whose delivery is `emailed` has a **non-captive** recipient and therefore no `clientUserId`. A recipient
  view is only valid for a captive one, so these routes answer `409` rather than minting a URL DocuSign would refuse;
  that signer opens the link from their own inbox instead.
- **`emailed`** — DocuSign emails the client a signing link. From the Project checkout, start the notation with
  `navigator project onboard --delivery emailed`. Complete intake, render the letter, and review the PDF. Dispatch the
  reviewed letter with `navigator site notation send <notation-id>`. The command prints the workflow state and provider
  request ID. Retrying a notation with an existing envelope returns its existing request ID.

## Completion webhook + HMAC

DocuSign Connect POSTs to `/webhook/esignature/:secret`. The handler verifies the raw-body HMAC
(`X-DocuSign-Signature-1`) **before** parsing, journals the verified receipt, classifies the event (`completed` →
`signature_received`; `declined`/`voided`/`expired` → `signature_declined`; everything else → 200 no-op), and consumes
it through one resumable path: signal the workflow, stamp the signature row, and — on completion — archive the signed
PDF + Certificate of Completion to object storage.

Journaling is what makes consumption durable (ENG-787): the verified receipt is written **before** it is consumed, a
mid-delivery crash or a failed phase leaves the receipt pending with the failure named, and every boot replays pending
receipts through the same consumption path — delivery is recoverable, not best-effort. A redelivery of an
already-consumed event is answered from the journal with 200 and the workflow is never signaled twice.

A `recipient-completed` event signals nothing. It reads the event's `recipientId` (the signer's routing order) back
against the template's `signers:` order and records that role in `signature.completed_signer_roles`. Once `client` is
recorded on a live envelope, the matter page reads "You have signed. Awaiting the firm's countersignature." and stops
offering **Review and sign**.

## Production cutover

Set up the Firm's live account separately from the demo account. Complete DocuSign Go-Live, grant JWT consent for the
Firm's API user, and get the REST base from `/oauth/userinfo`. Save the [Firm connection](#firm-connection) and
configure [account completion filing](#account-completion-filing). Apply PostgreSQL migrations before starting the
completion worker and verify the full flow against a synthetic Project on staging.

## Related

- [`third-party-integrations.md`](third-party-integrations.md) — the per-deployment provider convention this follows.
  [`.env`](../.env.example) is the canonical per-variable reference (JWT-grant preferred, static fallback).

## Firm connection

A Firm administrator saves a `DocuSign connection` in the Firm's integration secret settings. The value is a single JSON
object with `account_id`, `integration_key`, `user_id`, `private_key_pem`, `base_url`, `signer_name`, and
`signer_email`. Use the account's DocuSign REST base ending in `/restapi`, and encode PEM line breaks as JSON escapes.
Grant the integration JWT impersonation consent for the selected DocuSign user before connecting it.

Navigator uses the Project's owning Firm to resolve the encrypted connection. DocuSign calls use short-lived JWT grant
access tokens. Simulated deployments require a DocuSign demo account and live deployments require a live account.
Replace the secret to reconnect or rotate the key. Revoke it to stop subsequent operations through that connection.
Before switching an existing deployment to this build, provision the Firm connection for each account with outstanding
envelopes.

## Account completion filing

Add `connect_hmac_key` to the Firm's connection JSON and set the same key in DocuSign Connect. Configure an account-wide
JSON callback at `/webhook/esignature/firm/<firm-id>`. Subscribe to envelope sent, completed, delivered, declined,
voided, and expired, and recipient completed. The endpoint verifies the raw signature and account ID and journals
completed envelopes in PostgreSQL before acknowledging them. The Firm connection's DocuSign user needs permission to
read envelopes sent by the other users in that account. Connect can omit document bytes.

A verified progress callback reads the envelope and its ordered signers from the Firm account. Uniquely matched
envelopes appear under Documents on the lawyer matter page and in `navigator project status`, including envelopes sent
from the DocuSign web app before a Notation exists.

The worker verifies completion with DocuSign, then matches the envelope to a pitch or open Project in that Firm. It uses
the `project_code` envelope custom field, then a recognizable `<project-code>__` filename prefix, then the signing
client DRI's email. A known client DRI must be a signer even when a code selects the Project. Missing and ambiguous
assignments appear under **DocuSign completion review** on the Firm's admin page for manual verification and adoption.

The worker files the signed PDF and the Certificate of Completion with the verified envelope ID. For an onboarding or
engagement document, Navigator links the completion to the onboarding Notation and marks the engagement official after
filing. Other matched agreements are filed privately without making the engagement official. A notice goes to the
Project's private Slack channel after filing. A scheduled account poll recovers missed callbacks using a durable cursor.
