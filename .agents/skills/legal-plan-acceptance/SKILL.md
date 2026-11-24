---
name: legal-plan-acceptance
description: >-
  Reconcile LegalEASE acceptance notices with the provider portal, live Navigator Projects and workspace checkouts. File
  the originals and verify Gmail archive status when requested.
---

# Reconcile LegalEASE acceptances

Use the Gmail connector for all mailbox operations. Keep outgoing email draft-only. Search across labels.

```text
in:anywhere from:(legaleaseplan.com) subject:"LegalEASE ACCEPTANCE OF MEMBER CASE"
```

Follow pagination. Keep each message ID and thread ID. Count Projects by displayed case number and notices by message
ID. Distinguish Inbox, archived, snoozed, spam and trash. An archive label check alone cannot establish that the Project
has the email.

Open the provider's View All Cases link from an authentic notice. Match the Accepted Cases list by displayed case
number, member and matter, including subsequent pages. The number in the link path is an internal identifier. Read the
intake to identify the represented person, jurisdiction and adverse parties. A member may be seeking help for a spouse
or child. Keep tokenized links within the provider session and the restricted original email.

Run `navigator sync --dry-run --host <host>` to compare participating Projects with local checkouts. Check other live
statuses before concluding a Project is missing. Match by plan case number when available, then verify identity and
matter facts. For an existing live Project missing locally, recover its checkout. If a new Project is authorized,
confirm the permanent code and obtain the lawyer's conflict clearance using the verified parties. Open the Project in an
empty destination.

```sh
navigator project scaffold --dir <workspace>/<code> --host <host>
```

Re-read the live Project and its local manifest. Keep each unresolved identity, conflict or duplicate visible for
review.

For a missing email, follow the Firm's workspace instructions for temporary contacts. Mark a placeholder as temporary,
keep it out of client-facing messages and invitations, and update the same Person when the real address is verified.

File **each distinct notice** in the matched Project through the Navigator CLI. Follow the message-processing skill.
Check `navigator mail parse --help` for this release's filing options. Verify the original bytes, internal visibility,
Project document pointer and Communication. On retries, reuse an existing filing by source message identity. Keep case
facts, contact details, provider credentials and raw mail out of shared skills and Git. Store only the permitted
document pointer in the private Project repository. Omit client names, case numbers and portal links from branch names,
commit messages and pull requests.

When the lawyer requests archiving, remove only the Inbox label on verified acceptance notices through the Gmail
connector. For a mixed thread, target only the matching message. Read the labels back, including for notices that were
already archived. Report portal cases, live Projects, local checkouts, filed originals and mailbox status separately.
Identify any notice held for review by case number and reason.
