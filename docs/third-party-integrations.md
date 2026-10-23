# Third-party integrations — one provider attachment per deployment

Neon Law Navigator talks to a handful of external services. They fall into two kinds:

- **Binding vendors** perform real, billable, or legally binding actions on the firm's behalf — DocuSign for
  e-signature, Xero for accounting and billing. Every cloud deployment receives its own provider attachment and
  credentials. Staging attachments point only at vendor sandboxes; production attachments point only at live accounts. A
  vendor tenant may contain more than one attachment when that is the provider's native isolation seam, but keys, users,
  webhooks, and data partitions are never copied between deployments.
- **Platform services** are the cloud infrastructure the app runs on — durable execution, object storage, the database,
  identity, the agent-router LLM, and outbound/inbound email. They follow the same deployment boundary: one database,
  bucket set, OAuth client pair, Restate environment, mail credential, and webhook configuration per deployment.

The [full catalog](#current-integrations) below lists every external service the application code itself dials. Purely
operational layers that sit *above* the env-var interface — the SOPS-encrypted `deployments/` tree (secret values),
DNSimple (DNS) — are deliberately out of scope here: they are not code dependencies, and a fork can swap them freely.

## Why one attachment per deployment

- **No legal or financial weight in dev.** A test envelope or a draft invoice created against the sandbox account is not
  a binding signature or a real ledger entry. A leaked dev key cannot mint a production signature request.
- **Clean books and clean signers.** Test data stays out of the real accounting ledger and off real signers' inboxes.
- **Fault isolation.** Rotating or revoking one deployment's credential cannot take down another site.
- **Self-testable forks.** An OSS adopter can stand up their own sandbox account and exercise the full flow without
  touching a real account or paying for live API calls.

## Deployment selector and credential separation

The narrow deployment selector is exact: `NAVIGATOR_ENVIRONMENT=dev` selects the one development profile, which local
KIND uses, while exact `production`, empty, or unset selects production, which every hosted deployment uses. `staging`,
`test`, mixed case, whitespace, and every other nonempty value fail configuration parsing. It controls infrastructure
safety checks only.

- `.env` holds the **sandbox** credentials and is auto-loaded on startup, so local dev and `cargo test` run against the
  vendor sandbox by default.
- `.env.production` holds the **production** credentials. It is gitignored by the `.env.*` rule and never committed. To
  run against production locally, source it over the defaults before launching the binary:

  ```bash
  set -a; source .env.production; set +a
  ```

Both sources use the same application variable names (`DOCUSIGN_*`, `XERO_*`, …). The operator also sets
`NAVIGATOR_CREDENTIAL_ENVIRONMENT` to `dev` or `production`; startup rejects a mismatch. Each deployment receives its
own set from its own namespaced Kubernetes Secret; no two deployments share one.

## Firm-owned integration credentials

Notion and other Firm integrations are not deployment-wide credentials. A Firm's Admin DRI writes a typed provider
secret at `/app/admin/firms/{id}` (the **Integration secrets** section — create, replace/rotate, and revoke; a value is
never shown again after it is submitted) through the Navigator secret boundary (`store::firm_secrets`); the value is
envelope-encrypted with the dedicated runtime KMS key and is never returned in JSON, logs, traces, or durable payloads.
The resolver selects the credential from the Project's `firm_id`, so a Project cannot fall back to another Firm or to a
deployment environment variable. Owner may view the same page's metadata (kind, status, version, last-updated) and
appoints the DRI, but Owner is not a secret writer — `ManageIntegrationSecrets` admits only the Firm's own Admin DRI,
which an Owner may itself be.

The runtime KMS coordinate is `NAVIGATOR_RUNTIME_KMS_KEY`. It must name a dedicated runtime key, never the deployment
configuration key. Staging manifests and operator documentation may describe the workload permission; this repository
does not apply IAM or write cloud state. Provider clients receive a resolved credential through an injected trait and
tests use fakes, so local verification needs no live provider account.

`NAVIGATOR_RUNTIME_KMS_KEY` and `NAVIGATOR_NOTION_DATABASE_ID` are environment variables on the deployment secret. `web`
and `workflows-service` receive them through `envFrom`. The Deployment manifest names neither value. The runtime KMS key
stands alone: it also seals Brand social account tokens, so a deployment may carry it without Notion. A Notion database
requires the runtime KMS key. Social app credentials are described in
[`deployment-secrets.md`](deployment-secrets.md#brand-social-accounts-and-runtime-kms).

Notion reconciliation uses the explicitly selected `NAVIGATOR_NOTION_DATABASE_ID`. A missing, duplicate, renamed, or
archived page is an operator-visible repair outcome; the reconciler never silently creates a second page or replaces one
it cannot positively identify. It writes the canonical Project code and URL; page sharing and manual Notion fields are
out of scope for this surface (no invitations, no content mirroring) and untouched by it.

### The Firm integration doors

The consolidated setup command is the CLI client for these admin-tier provider doors:

| Command | Door |
| --- | --- |
| `navigator project setup <code>` / `--all` | Authenticated Drive, GitHub, Slack, and Notion doors |

They carry their own noun rather than nesting under `projects`, for the reason `project-surfaces` does: the `projects`
policy rule admits any authenticated caller several segments deep, so a provisioning path nested there would be
policy-reachable by a client even though the handler refuses one. The tier matches `project-surfaces` too — creating or
adopting a Project's external resources is one kind of act — and `--all` sweeps only the Projects visible to the calling
login, never every row in the deployment.

See [`project-repositories.md`](project-repositories.md#existing-project-setup) for the credential prerequisite, the
full outcome vocabulary, and how the CLI's exit code follows the typed outcome rather than the HTTP status.

Each provider response is one outcome slug per Project and nothing else. No page id, no channel id, no provider URL:
those are the Firm-private coordinates this boundary exists to keep on the firm side, and they are recorded on the
Project row for the surfaces entitled to read them. Setup reports `created` or `adopted` for provider resources and
`created` or `present` for Drive and GitHub. With no runtime KMS key, setup reports `runtime_not_configured` and never
falls back to a deployment-wide token; a Project can access only credentials its Firm wrote.

The Slack ensure door invites nobody — the adapter takes only provider-issued member ids, Navigator stores none, and it
will not turn a participation row or an email address into an invite, so the Firm's own Slack membership governs who
joins. The setup command retries each resource independently and exits nonzero for any required failure.

The persistent staging deployment (`NAVIGATOR_ENVIRONMENT=production` and `NAVIGATOR_SIMULATED_MATTERS=true`) appends a
final `from Staging` line to every outbound Slack body: the ops incoming webhook, the per-Project bot, and Firm-private
`chat.postMessage` calls. Restate jobs do not add a second staging parenthetical to those bodies. Production holding
real matters does not. Local KIND is `dev` and does not. GitHub Actions deploy narration is a workflow secret, not this
runtime path.

Inbound summary matching reads `NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS` from the deployment config (comma-separated SMTP
envelope recipients). Set it to the final recipient SendGrid receives after any Google Workspace routing rewrite
(normally the Parse intake address), not the visible `To:` header or original support mailbox. There is no compiled
default. Unset leaves the signed summary lane off; a present blank value fails boot. The lane is enabled only by
`NAVIGATOR_SUMMARY_ENABLED=true`, which also requires SendGrid's signed Inbound Parse public key, a Slack channel, the
HTTPS Restate broker, and Vertex AI access for the runtime service account. The web process captures the immutable
provider settings in the Restate request; the worker reads the same gate before registering provider adapters.

Texts enter the Firm Inbox through the Twilio number's "A message comes in" webhook (HTTP POST) at
`https://<NAVIGATOR_PUBLIC_HOST>/webhook/twilio/sms`. Set `NAVIGATOR_SMS_NUMBER` in E.164 format and put
`TWILIO_AUTH_TOKEN` in the deployment Secret. Navigator verifies `X-Twilio-Signature` against the configured URL,
resolves the registered receiving Brand's owning Firm, and saves one Message per `MessageSid`. The optional summary lane
also archives a `text/plain` receipt under `sms:<number>` and starts `EmailSummary`. Attachments stay in Twilio; the
Inbox and summary record their count. Inbox intake works when summaries are disabled. An unset number keeps the route
off; a configured number without an auth token or public host fails boot.

Authorized Project participants can reply through the same receiving number when `TWILIO_ACCOUNT_SID` is also
configured; a configured number makes `ops ship` require it. The Owner, or an Admin who may manage Brand content, can
also start a text to a new number from `/app/inbox` ("New text") and reply to any text conversation there. Live sends
require a production deployment with simulated matters disabled; a deployment on simulated matters (persistent staging)
texts only the numbers in the `NAVIGATOR_SMS_TEST_RECIPIENTS` secret, so a test never reaches a real person. Twilio
acceptance stores the provider SID; signed callbacks at `/webhook/twilio/status` update delivery on that Message.
Unknown sends are not retransmitted automatically. Operators must complete Twilio's applicable sender registration
before enabling replies. A text whose number matches a Person's recorded phone (digits only, ignoring a leading US `1`)
shows that Person and their role in the Inbox; the Owner or a Brand-content Admin links an unknown number from the
thread by entering the Person's email. A phone is an address, not proof of identity, and grants nothing.

Inbound Slack notices retain their state and receipt on Message. The configured messages channel belongs to
`SLACK_MESSAGES_FIRM_ID`, or the Firm wearing Neon when unset. The scheduled reconciler recovers pending notices and
checks ambiguous sends against a bounded `conversations.history` read. This requires the channel's history scope; an
absent correlation or missing scope leaves the notice unknown without posting again.

Normal staging requires real non-production SendGrid and DocuSign demo configuration. Each cloud deployment uses the
matching attachment row described in [`provider-environment-parity.md`](provider-environment-parity.md). Only the
explicit `NAVIGATOR_CI_HARNESS=1` staging test surface may use in-process fakes; production rejects that flag.

## Current integrations

| Service | Purpose | Kind | Env prefix |
| --- | --- | --- | --- |
| DocuSign | E-signature | binding | `DOCUSIGN_*` |
| Xero | Accounting / billing (`ACCREC` invoices) | binding | `XERO_*` |
| Notion | Project pages, meeting transcripts, Calendar booking, firm planning | firm-owned | resolved by `firm_id` |
| Slack | Firm-private Project channels and mechanism notices | firm-owned | resolved by `firm_id` |
| Restate Cloud | Durable workflow execution (`workflows-service`) | platform | `RESTATE_*` |
| Google Cloud | Storage, OIDC, archive | platform | `NAVIGATOR_*`, `GOOGLE_OAUTH_*` |
| Vertex AI | A2A agent-router LLM (Gemini Flash in prod) | platform | `NAVIGATOR_GCP_*` |
| SendGrid | Outbound + inbound email | platform | `SENDGRID_*` |

Notes:

- **Firm-owned journal.** Navigator stores authoritative Journal Entries in PostgreSQL. Mercury reads fetch only
  explicitly selected operating-account transactions and attachments. Xero reads provide tenant-scoped accounting
  evidence. Other banks, including Bank of Montreal IOLTA accounts, use provider-neutral entries and statement evidence.
  Navigator initiates no payments or transfers. See [Firm journal](firm-journal.md).
- **Google Cloud is several spec-compliant touchpoints, not one SDK.** Object storage goes through the `cloud`
  crate's `StorageService` trait (GCS in prod, SeaweedFS in dev); the store is SurrealDB over
  `NAVIGATOR_SURREAL_ENDPOINT` (Surreal Cloud in prod); OIDC is Google Identity validated against `GOOGLE_OAUTH_*`;
  Drive REST v3 is import-only, never a live store or archive. See [`cloud/README.md`](../cloud/README.md) for the full
  resource map.
- **Vertex AI is pluggable.** The router is the `portal::agent_router::AgentRouter` trait — `GeminiRouter` (Vertex AI)
  in prod, `NullRouter` in KIND. Swapping to another LLM means a new `impl`, not a new vendor account.
- **Contract review is a second, independent Vertex AI consumer.** `portal::contract_review::GeminiContractReviewer`
  drives the lawyer-review gate's `analysis__contract_deviations` step with its own model
  (`NAVIGATOR_CONTRACT_REVIEW_MODEL`, default a GA Gemini 3.x Flash-tier model) on the shared `NAVIGATOR_GCP_PROJECT_ID`
  / `NAVIGATOR_GCP_LOCATION` coordinates; unconfigured, it falls back to the deterministic `StubContractReviewer` (KIND,
  OSS forks). It migrated off `gemini-2.5-flash` ahead of Vertex's Gemini 2.5 retirement (public retirement 2026-10-20,
  shutdown 2027-03-31). At published Vertex rates (confirm before relying on for budgeting): 2.5 Flash was `$0.30` / M
  input tokens and `$2.50` / M output tokens; the 3.5 Flash tier this reviewer now defaults to is `$1.50` / M input
  tokens and `$9.00` / M output tokens — roughly 3-5x, because it is the full (non-Lite) tier, not the cheaper Lite tier
  `cloud::DEFAULT_GEMINI_SUMMARY_MODEL` uses for email summaries. Gemini 3.x Flash-tier models are served only from the
  `global` location, not a region — a deployment that pins `NAVIGATOR_GCP_LOCATION` to a region for its other GCP
  resources (buckets, cluster) must override it to `global` for this reviewer to reach the new default model.

When you add a vendor, define its provider-native staging and production seams, give each cloud deployment a separately
revocable attachment, add a `<VENDOR>_*` block to `.env.example`, and document a real smoke test. Local development may
use a sandbox credential in `.env`; a cloud credential belongs only in its deployment's `secrets.enc.yaml`. A platform
service also needs a stub/local equivalent (SeaweedFS, the in-cluster store, the `NullRouter`) so a fresh checkout boots
and self-tests without a cloud account.

## What Notion holds

Notion is the Firm's own workspace, kept for its meeting transcription and Calendar. Each page belongs to exactly one
zone, and material never crosses from a matter zone into a planning page:

| Zone | Holds | Never holds |
| --- | --- | --- |
| Project private page | the matter's notes, transcripts, research | other matters, credentials, filed documents |
| Project shared page | what the lawyer chose to show that client | anything firm-only |
| Firm planning pages | marketing plans, campaign research, glossary copy | client names, matter codes or material |

- **A transcript lands on the page of the Project it concerns.** A call that belongs to no Project yet is not filed
  under a planning page; it waits in the meeting notes until its Project exists.
- **Signed, filed, and generated documents are Navigator Assets**, not Notion attachments. The private page may link
  to an Asset; it is not the record.
- **The public Calendar booking link is a public firm fact.** It may appear in locale copy; private calendars, event
  details, and attendee lists may not.
- **Nothing in Notion is copied into Git.** Page URLs, page ids, database names, and transcript text stay out of
  tracked files, commits, PRs, and fixtures, under [public contributor safety](public-contributor-safety.md). Test
  fixtures use synthetic page URLs.

Navigator records each Project page's address and nothing else; who may open a page is Notion's own sharing (see
[Resource](glossary/resource.md)).

## Not in this catalog — and why

A few external-looking things are deliberately absent. They are not third-party SaaS vendors, so the per-environment
account convention does not apply to them:

- **Authorization policy** is compiled into the Navigator web process with the Rust-native Regorus interpreter. It is
  application code, not a vendor account or separately operated service. See [`rego-policy.md`](rego-policy.md).
- **OIDC identity is already the Google Cloud row.** In production, sign-in is **Google Identity** (validated against
  `GOOGLE_OAUTH_*`) — counted under Google Cloud above, not as a separate vendor. **Rauthy** is its **non-production
  stand-in** (the staging/KIND OIDC provider), exactly as SeaweedFS stands in for GCS and the in-cluster store for
  Surreal SQL. The identity provider is pluggable and spec-compliant either way.
- **The `deployments/` tree and DNSimple** sit *above* the env-var interface — the repository holds each deployment's
  coordinates and SOPS-encrypted secret *values* (the app reads plain env vars; see
  [`deployment-secrets.md`](deployment-secrets.md)), DNSimple holds DNS records. Neither is a code dependency, and a
  fork can swap both freely.

## Related

- `.env.example` — the canonical per-variable reference; this convention is stated in its top "Conventions" block.
  [`oss-install.md`](oss-install.md) — the install walkthrough's env-configuration step.
  [`env-driven-devx.md`](env-driven-devx.md) — the broader "one config surface, three audiences" env philosophy.
