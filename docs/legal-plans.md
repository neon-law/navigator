# Legal plans

A [Legal Plan](glossary/legal-plan.md) is a prepaid legal-services program that refers its members to the Firm and pays
the Firm for covered work. A plan is metadata the Firm holds about one payor, and a plan matter is an ordinary
[Project](glossary/project.md) linked to that metadata.

Both halves live in the deployment's database, never in this repository:

- **The Firm's plan row** (`legal_plan`) records where the Firm logs in, who answers provider questions, and the
  acceptance and turnaround service levels. What the plan pays per benefit is its [Plan Fee
  Schedule](glossary/plan-fee-schedule.md).
- **The Project's plan columns** name that row and carry the plan's own identifiers for the matter. The most important
  is the [Plan Case Number](glossary/plan-case-number.md), which every plan email and claim cites. A Project with these
  columns unset is a matter no plan referred.

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
hudson legal-plans --firm <firm-id> --host <host> create --name ARAG \
  --portal https://www.araglegal.com/account --contact "866-272-4529" \
  --acceptance-sla-hours 24 --turnaround-sla-days 90
hudson legal-plans --firm "Example Practice PC" --host <host> list
```

`--firm` accepts a Firm UUID or a display name. Display names resolve case-insensitively in the caller's authorized Firm
directory. A Firm member without directory access can run `list` with the UUID.

`create` needs the `manage_legal_plans` capability, which only Owner and an Admin of the Firm hold.

## A plan matter, from referral to payment

1. **Case request.** The plan emails a case request that names its Plan Case Number. ARAG asks the Firm to answer
   *Interested* or *No thanks* in the portal. LegalEASE asks it to accept in LAMP, or to reply to a referral email.
   Unanswered requests expire.
2. **Selection.** The member chooses the Firm, and the plan confirms it. ARAG sends a CaseAssist confirmation, and
   LegalEASE sends an acceptance notice with the benefit code and claim steps.
3. **Open the Project and link it.** Run this once the matter is opened:

   ```bash
   navigator project legal-plan set --plan ARAG --case-number <plan case number> \
     --state wa --assigned-on 2026-11-02 --accepted-on 2026-11-04
   navigator project legal-plan show
   ```

   Only the flags given change, and an empty value (`--closed-on ""`) clears one. The plan name ignores case. A name the
   Firm has no row for is refused, and so is a plan of another Firm.
4. **Engagement letter.** The fee section says the plan pays, names the Plan Case Number, and states what the plan does
   not cover. The Firm tells the client in writing before doing uncovered work.
5. **Claim.** When the matter completes, or a plan's early-claim rule allows, file the claim the plan's way (see the
   table). Then record `--closed-on`.

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
