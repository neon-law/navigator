# Apache AGE graph: provisioning and cutover runbook

This runbook provisions the `navigator_graph` relationship graph in a deployment and moves the conflict check onto it.
It is a procedure for a human operator; nothing in it is applied automatically. **Production provisioning needs Nick's
explicit go-ahead** — staging first, then production, in that order, per
[deploy-staging](../.agents/skills/deploy-staging/SKILL.md) and
[deploy-production](../.agents/skills/deploy-production/SKILL.md).

The design behind this procedure: the [relationship graph migration
design](https://app.obvious.ai/p/surreal-graph-migration-PD4XHRyh) and the [Cypher parity
probe](age-cypher-parity-probe.md). The image facts live in [images/postgres/README.md](../images/postgres/README.md);
the retained StatefulSet in [docs/postgres-kubernetes.md](postgres-kubernetes.md).

## 0. What you are provisioning

One Apache AGE graph, `navigator_graph`, in the existing PostgreSQL 16.15 cluster. The engine pair (PostgreSQL 16.15 +
AGE 1.6.0) ships inside the digest-pinned `neon-law-postgres` image — there is nothing to install at the filesystem
level. A deployment moves onto the graph in four steps: extension and roles, migrations, backfill, then the web rollout
that reads and writes the graph. Each step verifies before the next begins.

The write posture after cutover: PostgreSQL is authoritative. The `entity_role` and `relationship` relational rows and
their graph edges land in PostgreSQL; the SurrealDB graph tables receive nothing. `relationship_log` is untouched by all
of this — it stays an append-only relational table in SurrealDB per its own contract.

## 1. Extension and roles (privileged init, superuser)

Creating the extension needs superuser, and every later step (migrations, `web`) runs as a non-superuser login. The
`navigator-postgres-roles` Job is the only workload that connects as the bootstrap superuser;
`k8s/postgres/access/roles.sql` is its idempotent payload and already does everything this step needs:

- creates and converges `navigator_owner`, `navigator_app`, `navigator_backup`, `navigator_dbos`;
- `CREATE EXTENSION IF NOT EXISTS age;`;
- `GRANT USAGE ON SCHEMA ag_catalog` to the application logins;
- `ALTER ROLE ... SET session_preload_libraries = 'age'` for `navigator_owner` and `navigator_app`, so Cypher parses
  in their sessions without a shared-library restart.

```bash
kubectl --namespace navigator apply -k k8s/postgres/access
kubectl --namespace navigator wait --for=condition=complete job/navigator-postgres-roles --timeout=15m
```

Verify before moving on (psql as the superuser, or any session that reads `pg_*`):

```sql
SELECT extname, extversion FROM pg_extension WHERE extname = 'age';   -- age | 1.6.0
SELECT rolname, rolconfig FROM pg_roles
WHERE rolname IN ('navigator_owner', 'navigator_app');                -- session_preload_libraries = age
```

If the extension row is missing, the Job did not converge — read the Job's logs before continuing. Do not create the
extension by hand; the next `roles.sql` run would then converge around it and hide the divergence.

## 2. Migrations (as `navigator_owner`)

`navigator-migrate` is the one operator entrypoint for schema; it reads the same discrete `NAVIGATOR_POSTGRES_*`
variables `web` does and is carried by the app image at `/app/navigator-migrate`. Two graph migrations are in scope:

- `20261031010000_create_navigator_graph.sql` — the graph, its nine labels, property indexes, and the three loader
  functions;
- `20261031020000_graph_conflict_names_and_runtime_grants.sql` — the conflict-name frontier reader, the vertex-ensure
  function, and the runtime DML/graph grants for `navigator_app`.

```bash
cargo run -p store --bin navigator-migrate        # every pending migration, then verifies
```

The graph migration bootstraps the extension under IF-NOT-EXISTS semantics as a guard for environments with no
privileged init step (CI fixtures, scratch databases). In a real deployment step 1 has already created it, and the block
is a no-op. Verify:

```sql
SELECT name FROM ag_catalog.ag_graph;                                  -- navigator_graph
SELECT name FROM ag_catalog.ag_label ORDER BY name;                    -- nine labels
SELECT proname FROM pg_proc
WHERE proname IN ('graph_load_vertices', 'graph_load_edges',
                  'graph_conflict_edges', 'graph_conflict_frontier',
                  'graph_ensure_vertices');                            -- five functions
```

## 3. Backfill and parity

The graph projects from the relational mirrors, so the relational tables must be whole first. The mirror backfill runs
the graph arm last, after every source group is converged:

```bash
navigator ops postgres backfill --dry-run --json          # the plan; writes nothing
navigator ops postgres backfill --batch-size 500 --json   # the run; resumable by run id
navigator ops postgres mirror parity                      # must be clean before you trust it
```

The graph arm rebuilds the projection wholesale — vertices keyed by the Surreal record UUIDs, so `source_id` provenance
chains stay traceable — then validates counts and endpoint existence and reports them in the run's JSON. A clean run
reports the graph validation alongside the relational parity counts. Exit status 0 requires zero drift; a failed row
list is in `mirror_backfill_failure` by table and UUID, never by name.

## 4. Staging soak, then production cutover

The conflict check defaults to the AGE engine (`NAVIGATOR_GRAPH_AUTHORITY` unset means `age`). Cut a deployment over by
rolling its `web` after steps 1–3 are done — there is no separate flag flip to make. Set the variable explicitly only to
pin the *old* engine:

| Value | Behavior |
| --- | --- |
| unset / `age` | AGE engine: reads walk `navigator_graph`; writes land the PostgreSQL row and the graph edge |
| `surreal` | the rollback path — frozen Surreal graph reads; Surreal rows again (limits below) |

Staging soaks first: seed the sample matters, open matters through the real flows, and watch the conflict findings on
the pre-matter checks (the intake walk, project conflict scans). CI runs the findings-level dual-engine parity suite on
every change — identical conflict findings on shared fixtures are a merge gate, and the [Cypher parity
probe](age-cypher-parity-probe.md) records the AGE constraints the traversal respects (no multi-type edge patterns,
hop-by-hop expansion, `size()`/`id(e)` forms).

Then production, with Nick's go-ahead, in the same order: roles Job, migrations, backfill, web rollout.

## 5. Rollback — and its honest limits

`NAVIGATOR_GRAPH_AUTHORITY=surreal` plus a `web` rollout is the rollback, but read the limits before you reach for it.
They follow from the write retirement: during the AGE-authoritative window, SurrealDB's graph tables receive nothing and
freeze at the moment of the flip.

- **Reads during rollback are frozen.** The Surreal engine serves the graph as it stood when the AGE window began.
  Edges recorded since then are invisible to a conflict check running on Surreal — a missed adverse relationship is
  exactly the failure the check exists to prevent. Time-box the rollback and treat every check it produces in that
  window as understated.
- **A Surreal catch-up has no automated path.** The relational rows written to PostgreSQL during the window are absent
  from SurrealDB's copies too. Restoring them is a manual one-off export from PostgreSQL and load into SurrealDB; there
  is no tool for it. Plan the catch-up before flipping, not after.
- **The flip forward is always safe.** The AGE graph and its relational tables stay authoritative and whole during a
  Surreal rollback; re-flipping to `age` needs no migration and re-reads current data.
- **Do not flip during a PostgreSQL outage.** Under the AGE engine an unusable PostgreSQL fails the write and the
  check loudly — that is the designed behavior, not a bug to route around. Flipping to Surreal mid-outage would restart
  Surreal writes that the mirror cannot reconcile later.

If the problem is the AGE *data* rather than the engine, the fix is a rebuild, not a rollback: re-run the mirror
backfill (step 3). The projection is a derived copy; a wholesale rebuild is the deduplicator of last resort, and the
conflict check reads edges by id, which a rebuild renumbers.

## 6. Upgrades: logical dump/restore, never `pg_upgrade`

AGE 1.6.0's system tables carry OID references that do not survive a major-version `pg_upgrade`. Upgrading PostgreSQL is
a logical dump/restore into a fresh cluster of the target major version, followed by the steps of this runbook from
section 2 onward: the roles Job (section 1) if the cluster is new, migrations, and a full graph backfill. The retained
StatefulSet's restore drill carries the database half; treat the graph rebuild as part of the drill's success criteria.
The [design document](https://app.obvious.ai/p/surreal-graph-migration-PD4XHRyh) records this constraint; do not "fix"
it by attempting `pg_upgrade` on a staging cluster to see — the probe documents that it is unsupported.

## 7. What this runbook does not do

- It does not apply anything to production. The branch that introduced this procedure staged no production change;
  every production step above is a human decision with Nick's go-ahead.
- It does not provision the AGE extension on an image that predates the graph work. Verify the deployment runs a
  digest-pinned image built after `images/postgres/` gained the AGE recipe.
- It does not schedule SurrealDB's removal. The graph tables are retired from the *write path*; the Surreal store keeps
  serving the rest of the application until the remaining table migrations complete, per the [design
  document](https://app.obvious.ai/p/surreal-graph-migration-PD4XHRyh)'s sequencing.
