---
name: observability
description: >
  How Neon Law Navigator emits telemetry and how to debug a durable-execution failure fast. Every binary shares one
  seam, `telemetry::init` (stdout logs, JSON in prod, plus OTLP traces and metrics when `OTEL_EXPORTER_OTLP_ENDPOINT` is
  set), and everything lands in BigQuery (structured logs via a Cloud Logging sink; traces and metrics via an OTel
  Collector to Cloud Trace and Monitoring). Trigger when adding a span, metric, or log field, instrumenting a handler or
  workflow, wiring a new binary's main, debugging a missing nightly or periodic job or a silent trigger, running
  `navigator ops doctor`, or touching the OTel Collector or BigQuery sink. The one rule: instrument identifiers and
  counts, never content. A notation_id, service, outcome, duration, or status code is fine; a client name, answer body,
  email address, or document body must never enter a span, metric, or log. Skip for one-off println debugging.
---

# Observability

**Identifiers and counts, never content.** A span attribute, metric label, or log field may carry a `notation_id`, a
`service` name, an `outcome`, a duration, or an HTTP status. It never carries a client name, an answer body, an email
address, or a document body. Telemetry leaves the firm's trust boundary; privileged client content does not. This is a
standing engineering- and legal-council order. When in doubt, log the id and look the rest up.

## The seam

Every service binary calls `telemetry::init("navigator-<name>")` once in `main` and **holds the returned guard to the
end of `main`**. The drop flushes batched export, which matters for short-lived trigger jobs. Never hand-roll
`tracing_subscriber` in a binary.

- `OTEL_EXPORTER_OTLP_ENDPOINT` **unset**: human `fmt` logs to stdout, no OTLP, zero cost (dev, CI, forks).
- **Set**: JSON logs to stdout (Cloud Logging parses them) **plus** OTLP traces and metrics to the collector.

Every JSON stdout line carries `release` (the Navigator version, e.g. `26.10.1`, from `NAVIGATOR_RELEASE_TAG`); OTLP
carries it as `service.version`. Filter with `jsonPayload.release="26.10.1"` to tie a log line to its code. A local
build omits it.

To wire a new binary, add `telemetry.workspace = true`, then bind the guard to a name in `main` (never `let _ =`, which
drops it instantly):

```rust
let _telemetry = telemetry::init("navigator-<name>");
```

Seven service binaries wire the seam: `web`, `workflows-service`, the four `*-trigger` jobs, and `redirect`. The
interactive CLIs, `cli` (navigator) and `navigator-lsp`, deliberately do **not**: their output is for a human at a
terminal or an LSP client over stdio. Wire every new service binary; leave a new CLI stdout-only.

## Instrumenting work

- Instrument the **one shared chokepoint**, not N call sites. Workflow triggers are instrumented once in
  `workflows::start_workflow` (span `workflow.trigger`, metric `navigator.workflow.trigger.fired{service,outcome}`).
- Both agent surfaces over the shared tool catalog are instrumented at their dispatch chokepoint: A2A in `portal::a2a`
  (audit spans), and `/app/mcp` in `mcp::server::handle_tools_call` (span `mcp.tool.call`, metric
  `navigator.mcp.tool.called{tool,outcome}`). Record tool name and outcome only, never the `arguments`.
- Spans: `#[tracing::instrument(skip(secret_or_body), fields(service = ..., key = ...))]`. Skip bodies and tokens.
- Metrics: model new ones on `telemetry::record_trigger_fired(service, outcome)`. Create the counter from
  `opentelemetry::global::meter(...)` and add `KeyValue` labels that are ids or enums only. The global meter is a no-op
  when OTLP is off.
- Events: `tracing::error!(service, status = code, "…")`. Fields are ids and counts; the message names the condition.

## Where it lands

Logs go to Cloud Logging and a **BigQuery sink**. Traces and metrics go to Cloud Trace and Monitoring via the **OTel
Collector**, whose **fail-closed redaction processor** (`allow_all_keys: false`) is the last line enforcing the
no-content rule. Telemetry is the operational half; the Iceberg archive is the data half. The landing map, sink, and
collector config live in [`docs/observability.md`](../../../docs/observability.md#where-it-lands-direct-openobserve).

**Locally**, OTLP export goes to an in-cluster **OpenObserve**. `navigator dev up` wires host and in-cluster `web` to it
and writes the UI URL, OTLP endpoint, credentials, organization, and stream into `.devx/env`, on per-worktree ports. To
see what you emitted, follow
[`docs/observability.md`](../../../docs/observability.md#seeing-telemetry-locally-openobserve).

## Debugging a missing periodic job

1. **`navigator ops doctor`**: names any trigger Job wedged in `ImagePullBackOff` or `CrashLoopBackOff`, or active too
   long (which, under a `CronJob`'s `concurrencyPolicy: Forbid`, silently skips every run), and any unready workload,
   each with the exact `kubectl` fix.
2. **BigQuery or logs**: query `navigator.workflow.trigger.fired` by `service` and `outcome`. `rejected status=401` is a
   stale `RESTATE_AUTH_TOKEN`; `404` is a service not re-registered; `transport_error` is a hung or unreachable ingress.
3. **Restate Cloud console, Invocations**: did the worker run it, and which step failed.
4. **`Heartbeat` Slack line** (six-hourly, DBOS in `web`): its absence means durable execution itself may be down.

See [`docs/durable-workflows.md`](../../../docs/durable-workflows.md) for the durable-execution model.
