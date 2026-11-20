-- Restore drill report: recovery point, databases, extensions, per-table row
-- counts, and Apache AGE graph sizes. Compare it with the same report taken
-- from the live server for the chosen recovery point.
\pset footer off

-- The release manifest is generated from the SAME Rust admission builders;
-- persisted provenance is never treated as its own expected contract.
SELECT set_config('navigator.restore_contract', :'native_origin_contract', false) IS NOT NULL AS bound_contract \gset
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
DO $origin$
DECLARE
    expected JSONB := current_setting('navigator.restore_contract')::jsonb;
    base RECORD;
    step RECORD;
    effective JSONB;
    predecessor UUID;
    actual_name TEXT;
    actual_oid BIGINT;
    actual_marker TEXT;
    origin_rows BIGINT;
    upgrade_rows BIGINT;
    visited BIGINT := 0;
BEGIN
    IF to_regclass('public.database_origin') IS NULL THEN
        IF current_database() ~ '^navigator_native_' THEN
            RAISE EXCEPTION 'Native restore lacks Database Origin';
        END IF;
        RETURN;
    END IF;
    EXECUTE 'SELECT count(*) FROM public.database_origin' INTO origin_rows;
    IF origin_rows = 0 THEN
        IF current_database() ~ '^navigator_native_' THEN
            RAISE EXCEPTION 'Native restore lacks Database Origin';
        END IF;
        RETURN;
    END IF;
    IF origin_rows <> 1 OR expected->'format_version' IS DISTINCT FROM '1'::jsonb
       OR jsonb_typeof(expected->'schema_contract') IS DISTINCT FROM 'array'
       OR jsonb_typeof(expected->'authority_contract') IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'Native restore contract is absent or invalid';
    END IF;
    IF jsonb_array_length(expected->'schema_contract') = 0
       OR jsonb_array_length(expected->'authority_contract') = 0 THEN
        RAISE EXCEPTION 'Native restore contract is empty';
    END IF;
    EXECUTE 'SELECT * FROM public.database_origin' INTO base;
    SELECT datname::text, oid::bigint, shobj_description(oid, 'pg_database')
      INTO actual_name, actual_oid, actual_marker
      FROM pg_catalog.pg_database WHERE datname = current_database();
    IF base.format_version <> 1 OR base.origin <> 'fresh_native'
       OR base.creation_operation = '00000000-0000-0000-0000-000000000000'::uuid
       OR actual_name <> 'navigator_native_' || replace(base.creation_operation::text, '-', '')
       OR base.database_name <> actual_name OR base.database_oid <> actual_oid
       OR actual_marker IS DISTINCT FROM 'navigator-native-origin-v1:' || base.creation_operation::text
       OR base.tier_id = '00000000-0000-0000-0000-000000000000'::uuid
       OR base.namespace_uid = '00000000-0000-0000-0000-000000000000'::uuid
       OR base.cluster_name !~ '^[A-Za-z0-9-]{1,63}$'
       OR base.authority_contract IS DISTINCT FROM expected->'authority_contract' THEN
        RAISE EXCEPTION 'Native restore has foreign identity or authority';
    END IF;
    IF EXISTS (SELECT 1 FROM public.mirror_backfill_run) THEN
        RAISE EXCEPTION 'Native restore mixes captured history';
    END IF;
    IF to_regclass('public.database_origin_upgrade') IS NULL THEN
        RAISE EXCEPTION 'Native restore lacks its upgrade relation';
    END IF;
    EXECUTE 'SELECT count(*) FROM public.database_origin_upgrade' INTO upgrade_rows;
    IF upgrade_rows > 1024 THEN
        RAISE EXCEPTION 'Native restore exceeds the upgrade verification bound';
    END IF;
    IF jsonb_array_length(base.schema_contract) = 0
       OR jsonb_array_length(base.schema_contract) > jsonb_array_length(expected->'schema_contract')
       OR EXISTS (SELECT 1 FROM jsonb_array_elements(base.schema_contract) WITH ORDINALITY AS initial(value, n)
                  WHERE initial.value IS DISTINCT FROM (expected->'schema_contract')->((initial.n - 1)::integer)) THEN
        RAISE EXCEPTION 'Native restore has an unknown base schema contract';
    END IF;
    effective := base.schema_contract;
    predecessor := base.creation_operation;
    LOOP
        EXECUTE 'SELECT * FROM public.database_origin_upgrade WHERE predecessor_operation = $1'
          INTO step USING predecessor;
        EXIT WHEN step.upgrade_operation IS NULL;
        visited := visited + 1;
        IF visited > upgrade_rows OR step.completed_at IS NULL
           OR step.creation_operation <> base.creation_operation
           OR step.upgrade_operation = '00000000-0000-0000-0000-000000000000'::uuid
           OR step.predecessor_operation = '00000000-0000-0000-0000-000000000000'::uuid
           OR step.upgrade_operation = base.creation_operation
           OR step.from_schema_contract IS DISTINCT FROM effective
           OR step.authority_contract IS DISTINCT FROM expected->'authority_contract'
           OR jsonb_array_length(step.to_schema_contract) <= jsonb_array_length(effective)
           OR jsonb_array_length(step.to_schema_contract) > jsonb_array_length(expected->'schema_contract')
           OR EXISTS (SELECT 1 FROM jsonb_array_elements(effective) WITH ORDINALITY AS old(value, n)
                      WHERE old.value IS DISTINCT FROM step.to_schema_contract->((old.n - 1)::integer))
           OR EXISTS (SELECT 1 FROM jsonb_array_elements(step.to_schema_contract) WITH ORDINALITY AS next(value, n)
                      WHERE next.value IS DISTINCT FROM (expected->'schema_contract')->((next.n - 1)::integer)) THEN
            RAISE EXCEPTION 'Native restore has pending or foreign upgrade history';
        END IF;
        effective := step.to_schema_contract;
        predecessor := step.upgrade_operation;
    END LOOP;
    IF visited <> upgrade_rows OR effective IS DISTINCT FROM expected->'schema_contract'
       OR public.database_origin_schema_contract() IS DISTINCT FROM expected->'schema_contract' THEN
        RAISE EXCEPTION 'Native restore schema or upgrade chain does not match this release';
    END IF;
    RAISE NOTICE 'Native restore origin, schema and completed upgrade chain verified';
END;
$origin$;


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

COMMIT;
