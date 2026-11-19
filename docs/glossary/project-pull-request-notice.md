---
title: "Project pull request notice"
description: "A Project pull request notice records where one pull request's Slack message lives."
---

The record of the one Slack message a Project repository's pull request keeps in the [Project](project.md)'s private
channel. The first report of a pull request posts the message and records its channel and timestamp under the Project
and the pull request number; every later report, the merge, and the close edit that message instead of posting another.
A message deleted in Slack is posted again and the record replaced. The notice lists each [Pointer](pointer.md) the pull
request files with its visibility, and calls out every one it makes `visibility: client` as a client release.

- Store: [`store::postgres::pull_request_notices`](../../store/src/postgres/pull_request_notices.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `project_pull_request_notice`.
