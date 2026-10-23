# Finance aggregate

A Project's fee waterfall, computed by a durable workflow and read back over the Project API. Built for the
`sample-finance` sample Project, but nothing in it is specific to that code: any Project with an input document under
`finance/<project code>/` can run it.

## Shape

```text
finance/<code>/input.json ──> FinanceAggregate (Restate) ──> finance_aggregate row (SurrealDB)
                                     │                    └─> finance/<code>/<run_id>.json (object storage)
                                     └─ spans: finance_aggregate.run > ingest, aggregate, store

GET /app/api/projects/{id}/finance  ──> latest row for the Project, in the report shape below
POST /app/api/projects/{id}/finance/run ──> starts one run (lawyer tier)
```

- **Workflow.** `workflows-service/src/finance_aggregate.rs`, Restate service `FinanceAggregate`, keyed per run. Three
  journaled steps: `ingest` (read and parse the input), `aggregate` (the arithmetic), `store` (object plus row). Each is
  idempotent, so a retry rewrites rather than duplicates.
- **Arithmetic.** `store/src/finance_aggregate.rs`. Overhead comes off collected fees; net splits between attorney and
  firm; the firm share splits between Lead and Rain Maker. Rounding is half up on overhead, attorney share and Lead
  share; net, firm share and Rain Maker share are the remainders, so the four pieces always add up to collected.
  Aggregates add each row's rounded pieces and never split a total again. A test runs the same synthetic input the
  portal's reference implementation uses and requires identical cents on every group.
- **Row.** Table `finance_aggregate`, one row per run keyed on the run id, read as the latest `computed_at` per Project.
  The report is stored as JSON text.
- **Bucket.** The worker's documents bucket (`NAVIGATOR_DOCUMENTS_BUCKET`, else `NAVIGATOR_STORAGE_BUCKET`), the same
  handle `web` reads. No new bucket or credential. The `finance/` prefix keeps these objects apart from document keys.
- **Telemetry.** Ids and counts only: run id, Project id and code, row counts. Never amounts or names.

## Input document

`finance/<code>/input.json` holds `states`, `verticals`, `attorneys`, `months` and `matters`. Every amount is whole
cents. A matter naming a month, state, vertical or attorney the document does not declare is refused, because it would
land in the totals and in no group. The percentages default to 28 overhead, 70 attorney and 80 Lead.

## Running it on a deployment

1. **Upload the input** with an operator's own credentials (the workflow reads, it never uploads):

   ```bash
   gcloud storage cp sample-finance-input.json \
     gs://example-bucket/finance/sample-finance/input.json --project example-bucket
   ```

2. **Roll the deployment** if the running worker predates the `FinanceAggregate` service. Review the offline local plan
   first; it does not verify the live cluster or release availability:

   ```bash
   navigator ops ship --deployment neon-law-stg --tag <release tag> --dry-run
   navigator ops ship --deployment neon-law-stg --tag <release tag>
   ```

   The schema applies on boot (version 63 adds the `finance_aggregate` table).
3. **Register the worker with Restate** if `ship` warned that re-registration failed. See "The registration gotcha" in
   [durable workflows](durable-workflows.md). A missing registration answers `404 service 'FinanceAggregate' not found`.
4. **Start a run** as a lawyer-tier session. The run reads `finance/<code>/input.json` already in storage:

   ```bash
   navigator site login --host <host>
   navigator project finance run sample-finance --host <host>
   ```

   It prints the `run_id` (`manual-...`). A `503` means the deployment has no Restate ingress configured.
5. **Verify the read:** `navigator project finance show sample-finance --host <host> --json` returns `"source":
   "workflow"` and the run id from step 4. "no run stored yet" before the run finishes is expected.
6. **Verify the trace** in Dash0, dataset `neon-law-stg`: search for the span `finance_aggregate.run` with the run id,
   and check its children `finance_aggregate.ingest`, `finance_aggregate.aggregate` and `finance_aggregate.store`. The
   run span carries `navigator.keep`, so the sampler keeps it whatever the sampled percentage. Restate Cloud does not
   export into the Dash0 dataset: the trace shows the worker's handler span under the web request, and the Restate
   invocation itself is only in the Restate Cloud console.

## Report shape

`GET /app/api/projects/{id}/finance` returns `source`, `run_id`, `computed_at`, `parameters`, `totals`, `by_month`,
`by_state`, `by_vertical` and `by_attorney`. Money fields are `matters`, `billed_cents`, `collected_cents`,
`overhead_cents`, `net_cents`, `attorney_share_cents`, `firm_share_cents`, `lead_share_cents` and
`rain_maker_share_cents`. An attorney group adds `home_state`, `rain_maker_matters` and `rain_maker_credit_cents`.
Groups with no matters appear with zeros.
