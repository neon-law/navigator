---
title: "Durable object ownership"
description: "Verify and establish PostgreSQL ownership of Notation and Project Slack workflows."
---

# Durable object ownership

The Notation and Project Slack DBOS adapters require a PostgreSQL ownership epoch. New databases initialize one during
provisioning. Existing deployments establish ownership once before their DBOS-only rollout.

## Operator database role

Use the installation's PostgreSQL migration login, `navigator_owner`, through the approved private operator environment.
The runtime login, `navigator_app`, can read ownership metadata. The database restricts changes to activation and
provisioning. Application-history admission still applies to the operator connection.

## Activate ownership

Keep the previous writers stopped through rollout and verify that pending work has drained before issuing the command.
It records the operator's decision in PostgreSQL and leaves retained source workflow state behind.

```bash
hudson ops postgres activate-dbos \
  --environment ENVIRONMENT --operator-writers-held --acknowledge-discard
```

The transaction refuses a conflicting epoch, prior partial import, or unfinished PostgreSQL Notation transition.
Repeating the same activation returns the existing receipt count. Historical Projects receive unresolved [Project Slack
delivery](glossary/project-slack-delivery.md) evidence. An operator can reconcile client-view notices. The runtime holds
historical main-push notices because it cannot establish which events were delivered. New Projects use native delivery
records. An existing PostgreSQL Notation machine can resume. A historical Notation with a missing machine remains
blocked.

Verify on staging that DBOS resumes an in-progress workflow under the same identity after the executor restarts and that
PostgreSQL records each committed effect once. Verify the production deployment before retiring its old services.
