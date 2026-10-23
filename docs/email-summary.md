# Email summaries

Support mail addressed to the configured envelope recipient is archived and keyed by a scoped digest. The durable
`EmailSummary` workflow is opt-in, but a Person record alone does not authorize sending message text to Gemini. The
worker skips every sender until an explicitly approved, verified identity can be checked against its Firm and provider
account. Intake requires SendGrid to report a passing DKIM signature aligned with the RFC From domain. The sender must
resolve to a Person with `email_confirmed`, and a Firm's Owner or Admin DRI must explicitly approve that mailbox for
this provider account and the exact assigned Project. Only that Project’s current Client DRI is eligible; the workflow
checks current participation at admission and immediately before each model request. The optional
`NAVIGATOR_SUMMARY_MATTER_PROJECT_ID` is absent when no Project is assigned, so the workflow skips model processing.
Approval and revocation are recorded in an append-only audit table. The workflow contract and model request/parse
contract are in [`durable-workflows.md`](durable-workflows.md).

## Sender acknowledgment

Mail to `support@neonlaw.com` or `staging@neonlaw.com` is saved in the Firm's shared `message` transcript before an
acknowledgment is sent, in both the summary and ordinary inbound paths. Email and SMS already use this table; WhatsApp
transport and channel support are separate work. Registered receiving Brand mailboxes resolve their owning Firm;
`NAVIGATOR_INBOX_FIRM_ID` must agree with that ownership when set and may route an unregistered mailbox. Recipients from
multiple Brands are rejected rather than attributed arbitrarily. Missing ownership fails intake without sending an
acknowledgment.

The acknowledgment uses the shared Neon Law email header and says, "We received your message and will respond soon." It
replies from the receiving mailbox (preferring the SMTP envelope over display headers), preserves email thread
references, and declares `Auto-Submitted: auto-replied`. Automatic responses, bulk/list mail, empty return paths, and
mail from either receiving mailbox are saved without an acknowledgment. Parsed-mode provider headers are retained for
these checks and for message identity.

Messages without a provider message ID use a content digest that includes attachments. A stable outbound `message`
reservation prevents duplicate acknowledgments on redelivery or concurrent requests. Provider acceptance records
`submitted` and the provider reference. An interrupted or failed send remains `unknown` and is not automatically resent:
verify the provider outcome before taking recovery action. This automatic acknowledgment confirms receipt only; it does
not authorize a Clerk's substantive reply or grant matter or promotional consent.

## End-to-end status

The `navigator dev email-summary-e2e` command still exercises the real intake and durable workflow topology on a
worktree KIND cluster. Its model-success and Gemini-failure expectations are not satisfied while the worker admission
gate requires verified Firm/provider-account approvals. The model-success scenario still needs an approved,
email-confirmed sender fixture for its synthetic provider account. Do not treat the command as a passing proof until the
KIND scenario succeeds.

The remaining topology contract is:

- Docker's *selected* engine must be OrbStack. The command reads it and never changes the global context.
- The worktree's cluster must exist (`navigator dev worktree-env up --runtime kind`) with the `workflows-service`
  `RestateDeployment` present.
- The command builds the worker image from the checked-out source under an immutable tag
  (`navigator-workflows-service:e2e-<12 hex>`, a hash of the commit plus every tracked change and untracked file), loads
  it into the cluster, points the `RestateDeployment` at it, and fails unless Restate discovery lists `EmailSummary` and
  every running worker pod uses that image.

What is real: the host `web` listener built from this branch, the in-cluster Restate broker and worker, SurrealDB,
SeaweedFS, and ClamAV. What is synthetic and owned by the command: the Vertex metadata token endpoint, Gemini
`generateContent`, and the Slack Web API, served by one host HTTP fixture. The worker reaches the fixture at
`host.docker.internal` through three overrides that are honoured only under `NAVIGATOR_CI_HARNESS=1` and refused
otherwise: `GOOGLE_METADATA_URL`, `NAVIGATOR_SUMMARY_VERTEX_BASE_URL`, and `SLACK_API_BASE_URL`.

Two CLI-owned TCP proxies sit between `web` and the Restate ingress and ClamAV so a scenario can make either unavailable
without touching the cluster.

Requests are signed with a synthetic P-256 key whose public half `web` is given as `SENDGRID_INBOUND_PUBLIC_KEY`. Every
message, sender, and identifier is synthetic.

- **Current admission:** an unapproved sender causes no Gemini request. The ordinary inbound message and Slack alert
  remain available through their independent paths.
- **Concurrent redelivery:** four POSTs with different multipart boundaries yield one receipt, letter, invocation, model
  request, and Slack post.
- **Rejected requests:** a tampered body, missing signature, wrong path secret, invalid envelope, and a ClamAV outage
  return 401, 401, 401, 400, and 503, with no receipt, model call, or Slack call.
- **Restate unavailable:** a 503 and a retained archived receipt; the replay returns 202 and reuses the receipt, letter,
  and archive.
- **Provider outcomes:** 429 then 5xx then success takes 3 requests; three 5xx take 3 requests and post a failure
  notice; a 400 takes 1 request; an invalid object, a truncated candidate, and repeated timeouts each post a named
  failure notice.
- **Slack drops the response:** the post is recorded once, the delivery becomes `unknown`, nothing is resent, and an
  explicit reconciliation confirms it.
- **Worker killed after the model step:** the journaled model step is not invoked again and the interrupted post is not
  resent. The delivery is recorded `unknown`, because an admitted send with no recorded outcome is listed by `ops
  email-summary quarantined`, and reconciliation confirms it.
- **Worker restart after confirmed delivery:** the redelivered message reposts nothing and calls no model.
- **Non-support mail:** the existing inbound path answers 200 and creates no summary receipt, model call, or Slack call.

There is one provider (Gemini), so "independent provider outcomes" reduces to the failure notice a failed provider
produces. The disabled-feature configuration is covered by the non-harness configuration and route tests, not by this
command.

## Live Vertex

The deterministic suite never calls Vertex. A live smoke needs staging-scoped credentials and model entitlement; without
them the live result is a cloud-readiness blocker, not a pass.
