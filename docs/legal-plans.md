# Legal plans

A [Legal Plan](glossary/legal-plan.md) is a prepaid legal-services program that refers its members to the Firm and pays
the Firm for covered work. A plan is metadata the Firm holds about one payor, and a plan matter is an ordinary
[Project](glossary/project.md) linked to that metadata.

Both halves live in the deployment's database, never in this repository:

- **The Firm's plan row** (`legal_plan`) records where the Firm logs in, who answers provider questions, and the
  response window and any agreed completion term. What the plan pays per benefit is its [Plan Fee
  Schedule](glossary/plan-fee-schedule.md).
- **The Project's plan columns** name that row and carry the plan's own identifiers for the matter. The most important
  is the [Plan Case Number](glossary/plan-case-number.md), which every plan email and claim cites. A Project with these
  columns unset is a matter no plan referred.

The default response is due at the same UTC time on the next weekday. Record a carrier-specific response window only
when one applies. Leave the completion term empty unless the Firm has agreed to one.

The application DBOS host runs `LegalPlanRetainers` daily at 00:00 UTC and catches up after a restart. It checks the
installation Firm’s open Projects with a Legal Plan or Plan Case Number against verified execution and existing
signature requests. For a matter with one published Project engagement template and an identified client, it opens the
questionnaire. Once the reviewed PDF is ready, it prepares the envelope for the lawyer. The daily result identifies
matters needing a client, a draft, or review. The lawyer reviews the document, recipients and signing fields before
sending through Navigator.

## The provider portals

These are the panels the Firm works, from each provider's public attorney pages. The Firm's own row holds its login and
account details; this table holds only what anyone can read on the provider's site.

| Plan | Where the Firm logs in | Plan Case Number | Provider support |
| -- | -- | -- | -- |
| ARAG | [araglegal.com/account](https://www.araglegal.com/account) | CaseAssist, `NNNNNNN-NNN` | 866-272-4529 |
| LegalEASE | LAMP, from each case-review email | Member case or Case ID | 866-208-5143, option 2 |

**ARAG.** The [attorney page](https://www.araglegal.com/attorneys) leads to the portal. There the Firm answers case
requests, accepts a case by entering its CaseAssist number, and files claims: Get Paid, search the CaseAssist number,
then submit. A claim is usually filed when the matter completes.

**LegalEASE.** The [provider page](https://www.legaleaseplan.com/provider-info) lists the desks. Case reviews arrive by
email with LAMP links; [LegalCORNER](https://legalcorner.legaleaseplan.com/) is the attorney directory. Claims go to
claims@legaleaseplan.com with the subject `Claim Submission <case number>`. Ask claimshelpdesk@legaleaseplan.com about
claim status, and ask providersupport@legaleaseplan.com before working hours the plan has not approved.

Record a plan once per Firm, with the portal and contact the Firm actually uses:

```bash
navigator legal-plans --firm <firm-id> --host <host> create --name ARAG \
  --portal https://www.araglegal.com/account
navigator legal-plans --firm "Example Practice PC" --host <host> list
```

`--firm` accepts a Firm UUID or a display name. Display names resolve case-insensitively in the caller's authorized Firm
directory. A Firm member without directory access can run `list` with the UUID.

`create` needs the `manage_legal_plans` capability, which only Owner and an Admin of the Firm hold.

## Configure filing rules from a private checkout

An Owner or Firm Admin can keep one YAML file per plan in a private Firm Library checkout beside its skills, or in
another approved private repository. Supply its path explicitly. The import updates only the selected Firm and can
validate the entire document without saving it.

```yaml
name: Example Plan
filing_rules:
  - benefit_code: EXAMPLE-REVIEW
    effective_on: 2026-01-01
    trigger: service_completed
    filing_window_days: 30
    filing_destination: https://claims.example/
    filer: Handling lawyer
```

```bash
navigator legal-plans --firm <firm> configure --file <private-yaml> --dry-run
navigator legal-plans --firm <firm> configure --file <private-yaml>
```

Use one benefit rule per exact code and effective date. The service date selects the latest applicable version when a
claim first records its rule. A version accepts identical retries. For revised terms, enter a new effective date. Use
`hours_exhausted`, `matter_closed`, or `hours_exhausted_or_matter_closed` when the verified agreement uses those
milestones. Omit `filing_window_days` if the agreement supplies no verified calendar-day window. The configured filer
and destination guide manual handling. Store member identifiers, claim evidence and credentials in the authorized matter
and provider systems. Keep negotiated plan terms private.

## A plan matter, from referral to payment

1. **Case request.** The plan emails a case request that names its Plan Case Number. ARAG asks the Firm to answer
   *Interested* or *No thanks* in the portal. LegalEASE asks it to accept in LAMP, or to reply to a referral email.
   Unanswered requests expire.
2. **Selection.** The member chooses the Firm, and the plan confirms it. ARAG sends a CaseAssist confirmation, and
   LegalEASE sends an acceptance notice with the benefit code and claim steps.
3. **Open the Pitch.** In the serving Brand's portal, open **Plan intake** from **Triage review**. Confirm the
   member's name and verified email if the notice lacks an identified Person. Review conflicts, confirm the Project
   code, and take the referral after the Firm decides to accept it. The Pitch records the Firm's Legal Plan, the
   provider's case number, and any member ID. If a staged row lacks a case number, verify it against the original
   notice. Confirm the type of work or leave it unclassified when the notice does not establish a supported type.

   ARAG and LegalEASE referrals open a [Legal Plan Onboarding](glossary/legal-plan-onboarding.md) Notation for lawyer
   review. The lawyer handles provider confirmation and member contact separately.

   For an existing Project, add the Firm's plan name under `project` in its private `navigator.yaml`.

   ```yaml
   project:
     legal_plan: ARAG
   ```

   Run `navigator project set --apply-legal-plan --dry-run` to verify the link, then run `navigator project set
   --apply-legal-plan` to apply it. The plan name resolves within the Project's Firm, and an existing link to a
   different plan requires review. A new scaffold copies the plan name from Navigator when the Project already has a
   link. `navigator project set --from-live` fills a missing name on an existing checkout.

   Set a Project's plan and verified benefit from its checkout. This also declares the Firm's canonical plan name in
   `navigator.yaml`. The Plan Case Number, member ID, intake dates, and verified benefit terms stay in Navigator.

   ```bash
   navigator project legal-plan set --plan LegalEASE --benefit-code SAMPLE-BENEFIT \
     --cap-shape hourly-cap --cap-minutes 240 \
     --plan-rate-cents-per-hour 9000 --paralegal-rate-cents-per-hour 4500
   ```

   The example uses synthetic terms. When setting a cap shape, enter the complete terms from the verified authorization.
   If the engagement separately establishes a member-paid hourly rate, include `--member-rate-cents-per-hour` with those
   terms.

   `navigator project legal-plan show` displays the recorded terms.

4. **Engagement letter.** The fee section says the plan pays, names the Plan Case Number, and states what the plan does
   not cover. The Firm tells the client in writing before doing uncovered work.
5. **Claim.** When the matter completes, or a plan's early-claim rule allows, file the claim the plan's way (see the
   table). Then record `--closed-on`.

## Claims and collections

Start with `navigator billing`. **Needs my action** lists your time awaiting review and claims on matters where you are
the responsible lawyer. Review it with `navigator project time` or on the Project's Finances tab. **My time** shows your time
across matters. Run `navigator project billing` from the checkout for the matter view, or pass `--project
<code-or-uuid>`. The matter view shows the team's time and keeps review actions for staged time specific to the
signed-in timekeeper. Add `--json` for structured output.

### Record time from the CLI

Hourly benefits are claimed from recorded time. `navigator project time` records work that has no commit, such as calls,
emails, court appearances, and document review, and reviews it before it is billed. Run it from the Project checkout or
pass `--project <code-or-uuid>` and `--host <host>`.

```bash
navigator project time add --performed-on 2026-11-20 --minutes 30 --narrative "Call with opposing counsel"
navigator project time list --status pending-review
navigator project time edit <id> --minutes 18
navigator project time approve <id>
navigator project time approve <id> --charge plan-claim --claim <claim-uuid>
navigator project time discard <id>
```

`add` writes a [Time Entry Proposal](glossary/time-entry-proposal.md) for review; a past date marks it backfilled.
`--timekeeper <email-or-uuid>` credits another firm-side participant. Pass a stable `--request-id <uuid>` when a script
may retry: the retry returns the first proposal instead of adding a second. `approve` prices the time into a [Time
Entry](glossary/time-entry.md) and is safe to repeat. Staged Jujutsu time appears in the same list; approving it needs
`--narrative`, the invoice wording, and `--minutes` records less than the change attested. `list --status` reads
`pending-review`, `approved`, `unbilled`, `billed`, or `discarded`.

`navigator project time backfill` proposes entries from the Project's dated record: each Communication, filed document,
completed Deadline, and e-filing not yet proposed. It prints the default minutes it used. Add `--since <date>` to limit
the record and `--dry-run` to preview without writing. Nothing is approved until the lawyer reviews each proposal, and a
second run proposes nothing already proposed, approved, or discarded. Staged Jujutsu changes reach the list through
their `Billing-Time` trailers, not through backfill.

Run `navigator legal-plans --firm <firm> collections --filing-review` to see the number of claims awaiting filing review
and their verified dollar amount for ARAG and LegalEASE. The report includes the Projects you can access. Add
`--totals-only` for the Firm-wide view with plan-metrics access. Completed and prepared claims enter the count once
their fee terms establish an amount. Capped fees include only authorized minutes worked. A submitted claim leaves the
filing-review queue. The lawyer checks the carrier’s filing conditions before submission.

Projected fees use completed work and verified terms. For a plan claim, read the projected fee, amount filed, approved
outstanding balance, and net cash received as stages of the same claim. A payment reduces the outstanding balance. A
confirmed reversal reduces net cash. Work missing a verified fee stays visible for follow-up.

The contingency section shows the signed agreement's stage percentages and whether costs come out before or after the
fee. A lawyer's recovery estimate describes possible outcomes. Confirmed fees and recovered costs come from the recorded
matter outcome. A hybrid agreement may also authorize an hourly fee.

Record covered work as a [Legal Plan Claim](glossary/legal-plan-claim.md) on the Project, including after closure. Match
confirmation against the Firm, plan, Plan Case Number, and claim reference. If the reference or case is ambiguous, check
the provider record before recording evidence. A case number alone can identify several claims.

- **ARAG advice only.** For a covered consultation followed by declining representation, review the CaseAssist
  coverage, service date, and actual time. In Get Paid, find the CaseAssist case and look for Decline → Decline
  Services. Enter the advice date and actual time, and use an accurate decline reason. Ask ARAG for the correct option
  if none fits. Record the consultation benefit and use `arag_advice_only`; use `arag_representation` for covered work
  under an accepted representation. The attorney checks the current portal and attests before submitting.
- **LegalEASE.** Check the LAMP case and written authorization. Use `legal_ease_legal_guard` for an insured referral
  with its claim form, and `legal_ease_payment_authorization` for the separate non-LegalGuard route. Use the authorized
  fixed fee or approved rate and covered minutes for that case. Track additional minutes as pending until written
  approval. Follow the [provider's payment instructions](https://www.legaleaseplan.com/page/provider-info).

The lawyer submits through the provider's process. Keep any required receipt or supporting file under the approved
manual filing process. Navigator records the evidence reference, date, and amount when known.

Run these commands from the Project checkout or pass `--project <code-or-uuid>` before the action. Pass `--host <host>`
before the action if needed. The amounts in this example are synthetic.

```bash
navigator project billing work --service arag-advice-only --benefit SAMPLE-CONSULT \
  --performed-on 2026-11-01 --attorney-minutes 30 \
  --authorization-reference sample-benefit --fixed-cents 8000
navigator project billing prepare --claim <claim-uuid> --on 2026-11-01
navigator project billing submit --claim <claim-uuid> --on 2026-11-02 \
  --provider-reference sample-claim --claimed-cents 8000
navigator project billing pay --claim <claim-uuid> --on 2026-11-10 \
  --reference sample-remittance --cents 8000
navigator project billing
```

Use the claim UUID printed by `work` for later actions. If two claims share the same benefit, service type, and
completion date, give them distinct, stable `--work-key` labels. Retry an action with the same arguments and evidence
date. Use `--time-entry <uuid>` in place of `--attorney-minutes` to reuse recorded time. Use `--rate-cents-per-hour`,
`--covered-minutes`, and `--pending-minutes` for a capped authorization.

For structured imports, `navigator project billing record --input -` reads one JSON event from stdin. Use the same claim
UUID for every event on that claim and a new event UUID for each new action. A retry uses the original event UUID with
its original payload.

This synthetic ARAG consultation example supplies an explicitly verified fixed benefit. Amounts are integer cents. Use
existing `time_entry_ids` from the Project or enter actual `attorney_minutes`. For a capped authorization, use
`authorization.type = "capped_hours"` with `reference`, `rate_cents_per_hour`, `covered_minutes`, and `pending_minutes`.
If terms are missing, omit `authorization` and review the claim before preparation.

```json
{
  "claim_id": "959fd8d0-6a88-4f4d-9ed2-d02f6827d6e0",
  "event_id": "c030db25-a7fa-433d-ac9a-f6cc8b88d9f0",
  "occurred_on": "2026-11-01",
  "event": {
    "type": "work_recorded",
    "work": {
      "service": "arag_advice_only",
      "benefit": "SAMPLE-CONSULT",
      "performed_on": "2026-11-01",
      "attorney_minutes": 30,
      "authorization": {"type": "fixed_fee", "reference": "sample-benefit", "cents": 8000}
    }
  }
}
```

Reuse the outer fields with a fresh event UUID and the date of each action.

| Event `type` | Fields in `event` | When to record it |
| -- | -- | -- |
| `authorization_confirmed` | `authorization` | The provider verifies or revises the benefit terms. |
| `prepared` | None | The completed work has a verified payable amount and is ready for manual filing. |
| `submission` | `provider_reference`, optional `claimed_cents` | The lawyer confirms the filing. |
| `approval` | `reference`, `cents` | The provider confirms the amount approved. |
| `denial` | `reference` | The provider denies the claim or the remaining balance. |
| `correction` | `provider_reference`, optional `claimed_cents` | The lawyer confirms a corrected filing. |
| `payment` | `reference`, `cents` | Confirm one payment per remittance line. |
| `reversal` | `reference`, `cents` | A refund or reversal is confirmed. Use its own evidence reference. |

Use the personal worklist to review eligible completed work awaiting filing, missing terms, outstanding approvals, and
denials. Closed matters can still need a claim or payment reconciliation. Firm-wide totals remain available to operators
with the plan-metrics capability through the Firm collections report.

## Correspondence

Plan email is matter correspondence, so log it as a [Communication](glossary/communication.md) on the Project. Each
email is one row with `--source-ref gmail:<message id>`, so re-saving the same message returns the existing row:

```bash
navigator project communication create --project <code> --channel email_inbound --direction inbound \
  --counterparty <sender> --role other --at <RFC 3339> --subject "<subject>" \
  --source-ref gmail:<message id> --body-file <summary.md>
```

Case requests for referrals the Firm never accepted belong to no Project. Leave them in the mailbox.

## How the inbox routes plan email

Plan email names a matter only by its Plan Case Number. The plan's member name can differ from the client's, and a
client may write from an address the plan never mentioned. The [Omnichannel Inbox](glossary/inbox.md) therefore treats
the Plan Case Number as a matter identifier:

- **Route by number.** A subject or body that cites one Project's Plan Case Number for the sending plan suggests that
  Project with full confidence, ahead of the classifier.
- **Collapse case requests.** Case requests for numbers no Project carries are offers, not correspondence. The inbox
  groups them per plan with their expiry, apart from the matter queue.
- **Track plan status.** Request, confirmation, acceptance, and claim notices move one plan status per Project, so the
  matter view shows what is waiting on the Firm.

The Plan Case Number column and the `navigator project legal-plan` link come first, because they give every rule above
its key.
