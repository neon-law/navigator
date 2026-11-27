# DBOS workflow entry points

| Entry point | Work |
| --- | --- |
| Web-hosted schedules | Heartbeat, daily notices, Notation Reset, Project Recovery, retention, and configured exports |
| Kubernetes CronJobs | Nightly Project Update and YouTube Comments Poll |
| Authenticated application starts | Notation, Project Slack, alerts, triage, social publishing, and finance |
| Operator recovery | Email Summary redrive and the PostgreSQL recovery procedures |

[Durable workflows](durable-workflows.md) describes identity, ingress, and replay. [DBOS durability](dbos-durability.md)
describes database and executor ownership.
