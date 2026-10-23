---
title: Ship Transcript
description: "A Ship Transcript is the full output of one deploy roll, kept in a private sink instead of a public log."
---

## Ship Transcript

Everything one `navigator ops ship` printed, including what `kubectl` and `gcloud` wrote, saved as a single object in a
private sink. A Deployment Operator runs the ship with `--quiet --log-sink <URI>` when the runner's own log is readable
by people outside the firm, as an Actions log on a public repository is.

The console then carries one line: ok or failed, the exit code, a correlation id, a byte count, and a hash prefix. It
never carries anything the ship wrote. The correlation id is the Actions run id and attempt, so the transcript's key
follows from the run URL.

The sink is `gs://BUCKET[/PREFIX]` or `file:///DIR`. For GCS, setup grants the deployment's release identity
`roles/storage.objectCreator` with an IAM condition restricted to that bucket's `ship/` object prefix. The role cannot
read, list, overwrite, or delete an earlier Ship Transcript. A Deployment Operator reads a saved transcript with
`navigator ops logs --deployment <row> --run <run-id>-<attempt>`, optionally supplying the sink prefix. A Ship
Transcript can hold production pod logs, so each deployment uses its own project's private logs bucket, never a shared
one. A transcript that cannot be saved does not turn a successful ship into a failure, because the cluster has already
changed; the verdict says it was not saved.
