# Apache AGE Cypher-parity probe — conflict-check traversals

Phase-0 probe for the SurrealDB → Apache Age graph migration. Question: do the three traversal shapes
`store/src/conflicts.rs` runs today translate to openCypher on the pinned image with identical results? Answer: **yes —
all three translate, with documented workarounds for four AGE 1.6.0 Cypher gaps. No gap forces a recursive-CTE fallback;
the scoring Rust already owns stays in Rust.** Probe run Oct 30, 2026.

## Pinned versions (verified this probe)

| Component | Pin | Where pinned |
| --- | --- | --- |
| PostgreSQL | **16.15** | `images/postgres/Containerfile` (base `postgres:16.15-bookworm`, digest-pinned) |
| Apache AGE | **1.6.0** | same Containerfile — source tarball, sha512-verified |
| pgvector | 0.8.2 | same Containerfile |

The deployed image is `ghcr.io/neon-law-source-code/neon-law-postgres@sha256:4cc70fee…` (tag
`16.15-age1.6.0-vector0.8.2` in `k8s/postgres/postgres.yaml`), digest-pinned in `k8s/postgres/kustomization.yaml`.

Image-level compatibility (extension load, `create_graph`, restart persistence, dump/restore) is already evidenced in
[`postgres-compatibility.md`](postgres-compatibility.md) and CI-tested weekly via `images/postgres/tests/run.sh`. This
probe additionally **built AGE 1.6.0 from the same sha512-verified tarball against PostgreSQL 16.15** (PGDG packages,
same minor as the image base — no Docker in the probe sandbox) and exercised `cypher()` against it. The version pair is
proven end to end.

## Probe setup

Ground truth first: the fixture graph was loaded into SurrealDB **3.2.6** (the exact version in `Cargo.lock`) and the
*actual* `traversal_query()` / `edges_among()` SurrealQL from `conflicts.rs` was run against it. The same fixture was
then loaded into AGE and the traversals re-expressed in openCypher; edge sets compared.

Fixture (UUIDs stable across both engines, mirroring the `conflicts.rs` test scenarios):

- anchors `P1` (Fresh Client, person) and `E1` (Proposed Entity);
- `E1 -adverse_to(100)→ E2` (Existing Client Entity);
- `P1 -related_party(40)→ E2`;
- `P3 -[entity_role owns]→ E2`;
- `E1 -related_party(100)→ B -→ C -→ D -→ E` — a 4-hop chain whose last edge must stay outside `MAX_HOPS = 3`.

## The three traversal shapes

### 1. Bounded frontier walk (`traversal_query`, conflicts.rs:407-495) — ✅ translates

SurrealQL walks `MAX_HOPS=3` rounds of `<->entity_role, <->relationship` from the anchors and projects normalized edge
rows. openCypher equivalent:

```sql
SELECT * FROM (
  SELECT * FROM cypher('navigator_graph', $$
   MATCH (a)-[edges*1..3]-(x)
   WHERE a.id IN ['<anchor uuids>']
   UNWIND edges AS e
   RETURN DISTINCT id(e) AS edge_id, label(e) AS edge_type,
          e.kind AS kind, e.confidence_pct AS confidence_pct
  $$) AS (edge_id agtype, edge_type agtype, kind agtype, confidence_pct agtype)
) combined
-- normalization + label filter, SQL-side:
WHERE edge_type IN ('RELATIONSHIP', 'HAS_ROLE');
```

Result: **identical edge set** — 6 edges (5 `RELATIONSHIP` + 1 `HAS_ROLE`), the hop-4 chain edge excluded, and
mixed-label paths (adverse then `HAS_ROLE`) traversed correctly. Normalization of `entity_role` (implicit confidence
100, `role` as kind) is a `CASE`/`COALESCE` on `edge_type` — the `HAS_ROLE` arm carries `role` per the design's edge
mapping.

Two workarounds are load-bearing here (see gaps below): the match must be **unrestricted** (`-[edges*1..3]-`, no label
list — AGE rejects `[:A|B]`), and the **label filter happens SQL-side** after `cypher()`. Discipline to keep when the
design's `WORKS_ON` / `MEMBER_OF` / `HANDLES` labels land in the same graph: an unfiltered walk would collect them.

### 2. Scoring feed (paths for confidence product, adverse flags, floors) — ✅ translates; scoring stays in Rust

The Rust BFS (`reach`, `check`) consumes per-path kinds, confidences, and hop counts. openCypher hands it over as path
rows:

```sql
SELECT * FROM cypher('navigator_graph', $$
 MATCH path = (a)-[edges*1..3]-(x)
 WHERE a.id IN ['<anchor uuids>']
   AND NOT x.id IN ['<anchor uuids>']
 RETURN x.id AS target, x.name AS target_name, size(edges) AS hops, edges
$$) AS (target agtype, target_name agtype, hops agtype, edges agtype);
```

Each row carries the raw edge list (agtype JSON with `kind`, `confidence_pct`, `source_kind` per edge) — the shortest
path per target is `DISTINCT ON (target) … ORDER BY target, hops` SQL-side. Every input the scoring needs (confidence
product, `adverse_on_path`, `REVIEW_FLOOR_PCT` / `BLOCK_FLOOR_PCT` comparisons, revisit rule) derives from these rows in
Rust, reading `agtype` as text — exactly the design's plan. **This is confirmed as mandatory, not merely preferred:**
AGE 1.6.0 cannot compute the confidence product in Cypher (`reduce()` unsupported) nor the adverse flag (`any()`
unsupported).

### 3. Neighborhood read (`edges_among`, conflicts.rs ~954) — ✅ translates

One-hop undirected edges whose both ends are in the node set:

```sql
SELECT * FROM cypher('navigator_graph', $$
 MATCH (a)-[e]-(b)
 WHERE a.id IN ['<node uuids>'] AND b.id IN ['<node uuids>']
 RETURN DISTINCT id(e), label(e), e.kind, e.confidence_pct
$$) AS (…) WHERE label IN ('RELATIONSHIP','HAS_ROLE');
```

Result: identical to ground truth — 4 edges among `{P1, E1, E2, P3}`; chain edges excluded. (Per-label `UNION ALL` also
works for this shape since each edge is independent; only multi-hop paths need the unrestricted form.)

## Cypher parity gaps in AGE 1.6.0

| # | Gap | Severity | Workaround (verified) |
| --- | --- | --- | --- |
| 1 | No multi-type edge patterns (`[:A\|B]` won't parse) | Moderate | Unrestricted match + SQL-side label filter |
| 2 | No `reduce()` — confidence product impossible in Cypher | Low | Scoring stays in Rust, as the design places it |
| 3 | No `any()` / `all()` predicates | Low | Adverse flag derives Rust-side from the raw edges |
| 4 | `length()` rejects lists; `e.id` not projected; ORDER BY alias fails | Low | `size()`, `id(e)`, or order in SQL |

In detail: gap 1 holds even single-hop, so multi-label routes need the unrestricted form with a SQL-side `label` filter
— and once the design's `WORKS_ON` / `MEMBER_OF` / `HANDLES` labels land in the same graph, that filter is what keeps
them out of the conflict walk; per-label `UNION ALL` works where paths stay single-hop. Gap 3 also covers list
comprehensions over edge variables, which fail at runtime (`could not find properties for e`). Gap 4's ORDER BY error
(`could not find rte`) means ordering by the property directly or SQL-side.

None of these forces a recursive-CTE translation: the bounded walk, the scoring feed, and the neighborhood read all
express as Cypher `MATCH` statements, and the label-filter discipline in gap 1 is a query-shape rule, not an engine
failure.

## Upgrade-path implication

AGE's system tables carry OID references, so **`pg_upgrade` across PostgreSQL major versions is unsupported** —
confirmed against current AGE documentation. Every major-version bump is therefore a planned logical migration, not an
in-place upgrade:

1. Rebuild AGE against the *target* PG major first (its releases are
   per-major; check target compatibility before choosing the version).
2. `pg_dumpall --globals-only`, then `pg_dump -Fc` per database.
3. Restore with `CREATE EXTENSION age` before the data load; preserve
   `ag_catalog` and graph schemas.
4. Validate graph labels, indexes, sequences, and the probe's traversals
   before switching traffic.

This dovetails with the repo's existing evidence: `postgres-compatibility.md` already gates "graph traversal after
dump/restore", and `k8s/postgres/backup` + `restore-drill` cover logical/physical recovery — but the restore-drill
exercises same-major recovery. **A major-version dump/restore drill is a new gate** the migration plan should own before
cutover. The weekly image rebuild holds the server at the pinned 16.15, so minor-version drift is not a `pg_upgrade`
risk.

## Verdict

The pinned pair (PostgreSQL 16.15 + AGE 1.6.0) executes all three `conflicts.rs` traversal shapes as openCypher with
results identical to the SurrealQL originals on shared fixture data. The migration can proceed on Cypher — with gap-1's
label-filter discipline encoded in whatever query layer the port introduces, and a major-version dump/restore drill
added to the recovery gate before production cutover.

*Probe scripts (SurrealDB fixtures + ground-truth queries, AGE fixture + query suite) were run in the task sandbox and
are reproduced in full in this document; the only repo change is this note.*
