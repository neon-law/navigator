-- The Apache AGE relationship graph, projected from the relational mirrors.
--
-- `CREATE EXTENSION age` normally runs earlier, in the superuser init path
-- (k8s/postgres/access/roles.sql); this migration owns the graph itself so the
-- migration login owns every graph object. Environments that apply the
-- canonical schema directly with no privileged init step (CI fixtures, scratch
-- databases) bootstrap the extension in the block below under IF-NOT-EXISTS
-- semantics: a no-op when the init path already created it, and a loud
-- permission error for a non-superuser login when the init path never ran.
-- The loader helpers are the only
-- Cypher entry points for backfill batches: AGE's three-argument cypher()
-- accepts a bound parameter only as a bare Param node, so each helper takes
-- one JSON text payload, parses it into an agtype once, and passes it as a
-- PL/pgSQL variable — which the engine substitutes as exactly that Param.
-- The Cypher in each branch is a fixed dollar-quoted constant: no value ever
-- becomes query text.

-- The extension bootstrap lives in a DO block because the canonical-SQL
-- parser tracks procedural blocks as opaque and every bare statement it does
-- not know is rejected; the guard follows for the same reason.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'age') THEN
        CREATE EXTENSION age;
    END IF;
END
$$;

-- The guard lives in a DO block because the canonical-SQL parser tracks
-- procedural blocks as opaque and every bare statement it does not know
-- is rejected; the migration login owns the graph either way.
DO $$
DECLARE
    graph_id oid;
    label text;
BEGIN
    SELECT graphid INTO graph_id
    FROM ag_catalog.ag_graph WHERE ag_catalog.ag_graph.name = 'navigator_graph';
    IF graph_id IS NULL THEN
        PERFORM ag_catalog.create_graph('navigator_graph');
        graph_id := (
            SELECT graphid FROM ag_catalog.ag_graph
            WHERE ag_catalog.ag_graph.name = 'navigator_graph'
        );
    END IF;
    -- Every label exists before any load: validation and the property
    -- indexes read the label tables even when a family has no rows yet,
    -- and a label created lazily by the first batch would not exist on an
    -- empty projection.
    FOREACH label IN ARRAY ARRAY['Person', 'Entity', 'Project', 'Firm'] LOOP
        IF NOT EXISTS (
            SELECT 1 FROM ag_catalog.ag_label
            WHERE ag_catalog.ag_label.graph = graph_id AND ag_catalog.ag_label.name = label
        ) THEN
            PERFORM ag_catalog.create_vlabel('navigator_graph', label::cstring);
        END IF;
    END LOOP;
    FOREACH label IN ARRAY ARRAY[
        'HAS_ROLE', 'RELATIONSHIP', 'WORKS_ON', 'MEMBER_OF', 'HANDLES'
    ] LOOP
        IF NOT EXISTS (
            SELECT 1 FROM ag_catalog.ag_label
            WHERE ag_catalog.ag_label.graph = graph_id AND ag_catalog.ag_label.name = label
        ) THEN
            PERFORM ag_catalog.create_elabel('navigator_graph', label::cstring);
        END IF;
    END LOOP;
END
$$;

-- Runtime read access: cypher() executes as the calling login, so the app
-- needs USAGE on the graph's schema and SELECT on its label tables; writes
-- stay with the migration login through the loader helpers. The grant is
-- guarded because environments that apply the canonical schema directly
-- (CI fixtures) have no navigator_app role to grant to, and skipped when the
-- role is absent rather than failing the schema application.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        GRANT USAGE ON SCHEMA navigator_graph TO navigator_app;
        GRANT SELECT ON ALL TABLES IN SCHEMA navigator_graph TO navigator_app;
        ALTER DEFAULT PRIVILEGES FOR ROLE navigator_owner IN SCHEMA navigator_graph
            GRANT SELECT ON TABLES TO navigator_app;
    END IF;
END
$$;

-- Vertex batches. `kind` selects the fixed label; ids are the Surreal record
-- UUIDs, so relationship source_id chains and mirror parity stay traceable.
CREATE OR REPLACE FUNCTION graph_load_vertices(kind text, rows text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := rows::agtype;
BEGIN
    IF kind = 'person' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Person {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'entity' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Entity {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'project' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Project {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'firm' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Firm {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSE
        RAISE EXCEPTION 'unknown graph vertex kind %', kind;
    END IF;
END
$fn$;

-- Edge batches. Endpoints match by vertex id, so a batch may reference
-- vertices loaded in an earlier batch of the same transaction.
CREATE OR REPLACE FUNCTION graph_load_edges(kind text, rows text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := rows::agtype;
BEGIN
    IF kind = 'has_role' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:HAS_ROLE {role: row.role}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'relationship' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:RELATIONSHIP {kind: row.kind, confidence_pct: row.confidence_pct,
                                       source_kind: row.source_kind, source_id: row.source_id,
                                       detail: row.detail}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'works_on' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:WORKS_ON {role: row.role}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'member_of' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:MEMBER_OF]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'handles' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:HANDLES]->(b)
        $Q$, payload) AS (v agtype);
    ELSE
        RAISE EXCEPTION 'unknown graph edge kind %', kind;
    END IF;
END
$fn$;

-- Full projection rebuild: the graph arm clears and reloads inside one
-- transaction, so a run leaves either the old graph or the new one.
CREATE OR REPLACE FUNCTION graph_clear_navigator()
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$ MATCH (n) DETACH DELETE n $Q$)
        AS (v agtype);
END
$fn$;

-- One frontier hop of the conflict walk, both conflict edge types. AGE 1.6.0
-- rejects multi-type edge patterns (`[:RELATIONSHIP|HAS_ROLE]`), so each type
-- is its own fixed query and the results union in PL/pgSQL; the frontier
-- arrives as a bound parameter (an agtype object with a `frontier` id array),
-- never as query text.
CREATE OR REPLACE FUNCTION graph_conflict_frontier(frontier text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := frontier::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:RELATIONSHIP]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'RELATIONSHIP', kind: r.kind, role: null,
                         confidence_pct: r.confidence_pct, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r))}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r))}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- The bounded conflict walk: every conflict edge within 3 hops of the anchors,
-- expanded one frontier at a time exactly as the Surreal conflict traversal is
-- (store/src/conflicts.rs `MAX_HOPS` = 3; keep the two bounds in step). The
-- unrestricted variable-length walk is not used here: it would cross
-- participation edges (WORKS_ON, MEMBER_OF, HANDLES) that Surreal's walk never
-- follows, so the graph arm's parity check would disagree with the source
-- engine. Cross-hop duplicates are deduplicated by the caller.
CREATE OR REPLACE FUNCTION graph_conflict_edges(anchors text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    frontier jsonb := anchors::jsonb;
    round_edges jsonb;
    all_edges jsonb := '[]'::jsonb;
    visited jsonb := anchors::jsonb;
BEGIN
    FOR hop IN 1..3 LOOP
        EXIT WHEN jsonb_array_length(frontier) = 0;
        SELECT coalesce(jsonb_agg(DISTINCT edge), '[]'::jsonb)
        INTO round_edges
        FROM (
            SELECT (edge::text)::jsonb AS edge
            FROM graph_conflict_frontier('{"frontier": ' || frontier::text || '}') AS edge
        ) rounds;
        all_edges := all_edges || round_edges;
        -- An edge matched in an earlier hop can be matched again from its
        -- far side in a later hop (the frontier matcher is undirected), so
        -- dedup the accumulator, not just each round.
        SELECT coalesce(jsonb_agg(DISTINCT edge), '[]'::jsonb)
        INTO all_edges
        FROM jsonb_array_elements(all_edges) AS edge;
        -- Expand only unvisited endpoints: re-seeding the frontier with
        -- both endpoints of every found edge re-matched the same edge
        -- from its far side and accumulated duplicate rows per hop.
        SELECT coalesce(jsonb_agg(DISTINCT endpoint), '[]'::jsonb)
        INTO frontier
        FROM (
            SELECT edge->>'from_id' AS endpoint
            FROM jsonb_array_elements(round_edges) AS edge
            UNION
            SELECT edge->>'to_id'
            FROM jsonb_array_elements(round_edges) AS edge
        ) endpoints
        WHERE NOT visited ? endpoint;
        visited := visited || frontier;
    END LOOP;
    RETURN QUERY
    SELECT (edge::text)::agtype
    FROM jsonb_array_elements(all_edges) AS edge;
END
$fn$;

-- Loaders are the migration login's write path; the runtime login runs
-- read-only Cypher of its own and never executes these.
REVOKE EXECUTE ON FUNCTION graph_load_vertices(text, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_load_edges(text, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_clear_navigator() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_conflict_frontier(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_conflict_edges(text) FROM PUBLIC;
