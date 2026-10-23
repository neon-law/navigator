---
title: "Notice of Electronic Filing"
description: >-
  A Notice of Electronic Filing is the email a court's CM/ECF system sends for each docket entry, carrying one free-look
  link and the Service List.
---

The email a federal court's CM/ECF system sends to every registered recipient when a [Docket Entry](docket-entry.md) is
made. It names the court, the case number, the entry number, the docket text, the filer, and each document's original
filename. The bulk of its body is the [Service List](service-list.md).

The entry number links to a **free look**: one download of the document without a PACER fee. The link is single-use. It
closes after the first view or after fifteen days, so a failed run that spent it falls back to a paid download.

A text-only entry, such as a minute order, carries no document. Its notice reads "No document attached".

`navigator project docket ingest` reads saved notices for the cases a Project's `navigator.yaml` lists. It writes each
document's bytes to disk before it does anything else, then files them into that Project, and records the Service List
for conflicts. See [Project repositories](../project-repositories.md#court-notices).

- Parser: [`cli::projects::docket::nef`](../../cli/src/projects/docket/nef.rs)
