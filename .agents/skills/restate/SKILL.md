---
name: restate
description: Implement or review a Restate handler without breaking replay safety.
---

# Restate

Read [`docs/durable-workflows.md`](../../../docs/durable-workflows.md),
[`docs/agent-workflows.md`](../../../docs/agent-workflows.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- Put every non-deterministic operation behind the journaled boundary (`ctx.run`) with a stable name.
- Classify failures correctly, keep workflow steps durable, and add a covering test for changed behavior.
- Operational debugging logs identifiers and status only. Never emit client content, legal files, real contact details,
  or production identifiers.
- Debug a run with `restate -e <staging|production> invocations list --service <Name> --all`, then
  `sql "select index, name, entry_json from sys_journal where id='<inv>'"`. Step results are byte arrays; decode them. A
  completed run can hold a failed step, so read the journal, not the status.
- To find the code behind a run, match its `release` (see the observability skill) to a tag.
- Tell a model what JSON keys to return. A strict parser rejects invented ones.
- Never paste `restate config view`: it prints the access token.
