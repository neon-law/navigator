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

## Scheduled Inbox preview

Run `navigator inbox assign-matches --scheduled --dry-run` under the approved Navigator identity. The Portal requires
`NAVIGATOR_INBOX_PREVIEW_ACTOR_ID`, `NAVIGATOR_INBOX_PREVIEW_FIRM_ID`, `NAVIGATOR_INBOX_PREVIEW_BRAND`, and
`NAVIGATOR_INBOX_PREVIEW_PROVIDER_ACCOUNT`. Use the integration's recorded provider account for the receiving mailbox.
The current actor must have access to the configured Firm and the Firm must own the receiving Brand. The preview also
accepts Firm-only mail when the configured Firm and provider account match the lawyer intake's canonical mailbox.

The weekday pilot previews pending Navigator Inbox messages from that mailbox and only considers open Projects in the
same Firm and Brand. Its report contains message IDs, proposed Project IDs, and reasons when there is no unique match.
It leaves mail and Projects untouched. Review each proposal in Navigator and confirm each filing individually.

## Testing

`server/tests/twilio_summary.rs` exercises intake and the DBOS workflow against synthetic PostgreSQL data and local
provider doubles. The worker's tests cover sender approval, provider outcomes, and recovery. The KIND release gate
checks DBOS workflow continuity across an application restart.

## Live Vertex

The deterministic suite never calls Vertex. A live smoke needs staging-scoped credentials and model entitlement; without
them the live result is a cloud-readiness blocker, not a pass.
