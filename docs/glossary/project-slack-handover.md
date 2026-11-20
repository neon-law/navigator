---
title: "Project Slack Handover"
description: "A Project Slack Handover records one Project’s posting-evidence admission."
---

A Project Slack Handover binds a [Project](project.md) to its object ownership epoch and posting-evidence disposition.
Captured evidence remains inactive until verified activation. A new native Project initializes this record atomically
under an established native epoch; an empty target or a channel row does not prove that a legacy notice was never sent.

Client-view evidence is posted, verified not-posted or unresolved. Unresolved evidence fences that notice until an
attributed reconciliation. Main-push deliveries preserve the source’s bounded 200-event history, not an unlimited
no-repost guarantee. This per-Project record follows canonical Project deletion; the capture record remains separate.

The table is `project_slack_handover`.

Schema: [`project_slack_handover`](../../store/postgres/schema.sql).
