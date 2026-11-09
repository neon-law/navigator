# Observability overlay

This overlay connects Navigator workloads to the OpenTelemetry collector and the reference GCP telemetry sinks. It
defines the production shape for sampled diagnostics, searchable logs, long-term telemetry export, retention, and
self-monitoring.

It serves deployment and incident-response operators. The overlay is necessary because legal-service telemetry must be
useful without carrying client content: redaction fails closed, access is controlled as a privilege boundary, and the
unsampled archive remains distinct from operational views.

Treat this directory as operator-applied scaffolding, not an application default. The authoritative privacy,
configuration, and recovery requirements are in [observability](../../../../docs/observability.md) and [cloud
operations](../../../../docs/cloud-operations.md).

`dash0-check-rules.yaml` holds the Dash0 check rules for the Nightly Project Update: a degraded run and a missed run.
Apply it to the deployment's Dash0 dataset with the Dash0 CLI or the Dash0 operator. The file header gives both
commands. `dash0-forgejo-check-rules.yaml` holds the Forgejo Actions rules, applied the same way: a job waiting with no
runner, a stopped runner controller, a gate or CD failure on `main`, a merge with no CD run, leftover runners, and
missing Forgejo metrics. `dash0-archives-check-rules.yaml` holds the Archives rules: a failed run, and no archive table
written in 13 hours, which also catches a run that never started.
