---
title: "Member Contacts"
description: >-
  Member Contacts are the people on a firm member's open matters, kept in that member's own Google Contacts so Google
  Voice and Google Fi name the caller.
---

Every firm-side participant on a [Project](project.md), whether lawyer, clerk, or admin, gets a Google contact for each
other participant on each of their open Projects: name, email, and every [Person Phone](person-phone.md). Google Voice
and Google Fi both read the signed-in Workspace user's own contacts to name a caller, so one sync covers both. A client
gets none.

Navigator acts as each member through the deployment's Workspace service account, the same keyless delegation the
[Project Calendar](project-calendar.md) uses. The account's domain-wide delegation must grant
`https://www.googleapis.com/auth/contacts`. Each contact Navigator writes carries a `clientData` entry keyed
`navigator_person_id`. A member's own contacts carry none, and the sync never reads, changes, or removes them.

One member's book is reconciled whole: missing people are created, changed names, emails, and numbers are updated, and a
person on none of the member's open matters is removed, which happens when a matter closes. `navigator project setup`
runs the sync for every firm-side participant on the Project, and `--all` covers every visible matter.

- Provider: [`cloud::contacts`](../../cloud/src/contacts.rs) · reconcile:
  [`store::contact_sync`](../../store/src/contact_sync.rs) · door: `POST /app/api/integrations/contacts/sync`
