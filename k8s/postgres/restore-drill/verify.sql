-- Restore drill report: recovery point, databases, extensions, per-table row
-- counts, and Apache AGE graph sizes. Compare it with the same report taken
-- from the live server for the chosen recovery point.
\pset footer off

SELECT pg_last_xact_replay_timestamp() AS recovered_through,
       current_setting('server_version') AS server_version;

SELECT datname AS database
FROM pg_database
WHERE NOT datistemplate
ORDER BY datname;

SELECT extname AS extension, extversion AS version
FROM pg_extension
ORDER BY extname;

SELECT table_name,
       (xpath('/row/c/text()',
              query_to_xml(format('SELECT count(*) AS c FROM public.%I', table_name),
                           false, true, '')))[1]::text::bigint AS row_count
FROM information_schema.tables
WHERE table_schema = 'public'
  AND table_type = 'BASE TABLE'
ORDER BY table_name;

SELECT EXISTS (SELECT FROM pg_extension WHERE extname = 'age') AS has_age \gset
\if :has_age
SELECT g.name AS graph,
       (xpath('/row/c/text()',
              query_to_xml(format('SELECT count(*) AS c FROM %s._ag_label_vertex', g.namespace),
                           false, true, '')))[1]::text::bigint AS vertices,
       (xpath('/row/c/text()',
              query_to_xml(format('SELECT count(*) AS c FROM %s._ag_label_edge', g.namespace),
                           false, true, '')))[1]::text::bigint AS edges
FROM ag_catalog.ag_graph AS g
ORDER BY g.name;
\endif

-- DBOS durable state, when this database hosts the executor: run counts by
-- status and release, and which executor identity owns each pending run.
-- A recovered PENDING row resumes only on an executor with that identity and
-- release (docs/dbos-durability.md). Counts only; no workflow inputs or outputs.
SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname = 'dbos') AS has_dbos \gset
\if :has_dbos
SELECT status, application_version, count(*) AS workflows
FROM dbos.workflow_status
GROUP BY status, application_version
ORDER BY application_version, status;

SELECT executor_id, application_version, count(*) AS pending
FROM dbos.workflow_status
WHERE status = 'PENDING'
GROUP BY executor_id, application_version
ORDER BY executor_id, application_version;
\endif
