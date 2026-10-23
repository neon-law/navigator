# PostgreSQL compatibility evidence

The compatibility workflow uses the same immutable PostgreSQL image as the KIND overlay. The matrix is PostgreSQL 16.15,
Apache AGE 1.6.0, pgvector 0.8.2, SQLx 0.9.0, and the native DBOS 0.5.0 synthetic test application at upstream commit
`155a38bbec5d0cfcec0035c65b0c5c79012ab223`.

| Boundary | Evidence |
| --- | --- |
| SQLx pooling and values | UUID, TIMESTAMPTZ, JSONB, rollback and pool limit tests |
| Timestamped migrations | Concurrent runners, upgrade, checksum drift and failed-DDL recovery tests |
| AGE | SQLx text projection of `agtype`; graph traversal after dump/restore |
| Vector, text and case | Bound vector input, exact distance, full-text match and citext equality |
| Application upgrade | Core/Post schema on vanilla PostgreSQL; social and journal schema on the extension image |
| Image replacement | Base-to-extension image replacement preserves the same PVC and application rows |
| Database backup | Custom-format `pg_dump` and `pg_restore` into a separate disposable database |
| Physical recovery | `pg_basebackup` with streamed WAL; a second server restores SQL, AGE and completed DBOS state |
| Durable state | Dump includes DBOS's schema and an interrupted workflow; restored worker completes it |
| Side-effect recovery | Completed checkpoint executes once; interrupted step retries; unique effect remains once |
| Pod replacement | New pod UID, unchanged PVC UID, and restored SQL/AGE data |

The image pins are read from the recipe and overlay:

- Base digest: `sha256:0ea6700a3b4f0ae6ce746519073558aed4d88a79d8d07622a9a644946c7319c4`
- Target digest: `sha256:4cc70fee3aa9e53455bd1612d0a5be1f06962f5bcdcda4c6bff47e2b46db969e`

Run the focused Rust tests with `NAVIGATOR_TEST_POSTGRES_PORT` set to the isolated fixture's loopback port and
`NAV_REQUIRE_POSTGRES=1`. Build the upstream test application at the pinned commit, then run:

```bash
KUBECONFIG=.devx/kubeconfig bash store/postgres/tests/integrated_recovery.sh \
  kind-YOUR_TASK /path/to/pinned/tests/dbos/target/debug/dbos-image-smoke 25433
```

The script requires an explicitly named KIND context and uses only the `postgres-spike` fixture. It creates two
synthetic databases and deletes them at exit, restores the pinned image, and stops its own worker and port-forward. Run
it after other tests using this fixture because it replaces the database pod. The Rust type tests use their own
disposable databases.

CI uses `k8s/kind-postgres-config.yaml`, with no ingress host-port mappings. The pinned DBOS test application can bind
its readiness listener on port 8080 while the database uses an explicit port-forward.

This evidence covers the partial application schema, same-major image replacement, logical dump/restore and physical
backup recovery with streamed WAL. Full domain parity, a production DBOS adapter, major-version upgrades, GKE recovery,
scheduled backup retention and time-targeted PITR need their own gates. The database dump includes metadata and durable
state; it does not back up object-store document bytes. See [SQL tooling](postgres-tooling.md) for runner adoption.
