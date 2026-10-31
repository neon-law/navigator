-- The conflict traversal's runtime read shape, and the runtime login's graph
-- access.
--
-- 1. `graph_conflict_frontier` projects each endpoint's `name` beside its id
--    and label. The conflict check prints the parties a finding reached by
--    name, and the Surreal traversal dereferenced names in the same round
--    trip (`store/src/conflicts.rs` `TRAVERSAL_FIELDS`); the AGE walk reads
--    the same shape, so an explanation never depends on which engine served
--    it. `store/src/conflicts.rs`'s neighborhood builds identical adjacency
--    and labels from either engine's rows. The body is the graph migration's
--    function verbatim plus the two name projections; the signature and
--    return type are unchanged, so `CREATE OR REPLACE` rewrites it in place.
--
-- 2. `graph_ensure_vertices` covers the case the wholesale projection
--    cannot: the runtime write path records an edge whose endpoints may
--    have been minted after the last projection rebuild — the intake walk
--    creates a person and immediately records a relationship about them.
--    It reads back only what is already in the graph and creates at most
--    the endpoints of the one new edge, so the loader's CREATE stays safe.
--    `graph_load_vertices` and `graph_clear_navigator` stay with the
--    migration login: the projection rebuilds whole.
--
-- 3. The runtime login may execute the graph readers, the edge loader, and
--    the vertex-ensure the runtime write path needs. A conflict check on the
--    matter-open path runs as `navigator_app`, not as the migration login
--    that built the graph. Execute was revoked from PUBLIC by the graph's
--    own migration; these grants are the narrow re-grant, and they stay
--    guarded on the role's existence so a role-less schema apply skips them
--    unchanged. The catalog (`ag_catalog`) stays closed to the runtime
--    login; `cypher()` needs no catalog reads.
--
-- 4. The PostgreSQL-authoritative write path (the `age` engine authority)
--    inserts the relational source rows (`entity_role`, `relationship`)
--    and reads the endpoint names the vertex payload carries. The Surreal
--    engine path needs none of these: it writes through SurrealDB and
--    mirrors, as before.

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
                         from_label: label(startNode(r)), from_name: startNode(r).name,
                         to_id: endNode(r).id,
                         to_label: label(endNode(r)), to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), from_name: startNode(r).name,
                         to_id: endNode(r).id,
                         to_label: label(endNode(r)), to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

CREATE OR REPLACE FUNCTION graph_ensure_vertices(kind text, rows text)
RETURNS integer
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype;
    missing jsonb := '[]'::jsonb;
    source jsonb;
    vname text;
    found integer;
BEGIN
    IF kind NOT IN ('person', 'entity', 'project', 'firm') THEN
        RAISE EXCEPTION 'unknown vertex kind %', kind;
    END IF;
    FOR source IN SELECT * FROM jsonb_array_elements(rows::jsonb) LOOP
        payload := jsonb_build_object('id', source->>'id')::text::agtype;
        SELECT count(*) INTO found FROM cypher('navigator_graph', $Q$
            MATCH (v) WHERE v.id = $id RETURN v LIMIT 1
        $Q$, payload) AS t(v agtype);
        IF found = 0 THEN
            -- The caller's name wins — it reads the row that minted the
            -- vertex before PostgreSQL's copy exists. The fallback is the
            -- relational row's own name; a row the table does not have
            -- (deleted between the edge write and this load) projects
            -- with no name, and the explanation falls back to the id
            -- until the next rebuild refreshes names.
            vname := source->>'name';
            IF vname IS NULL THEN
                EXECUTE format('SELECT name::text FROM %I WHERE id = ($1)::uuid', kind)
                    INTO vname USING source->>'id';
            END IF;
            missing := missing || jsonb_build_array(
                jsonb_build_object('id', source->>'id', 'name', vname));
        END IF;
    END LOOP;
    IF jsonb_array_length(missing) > 0 THEN
        PERFORM graph_load_vertices(kind, missing::text);
    END IF;
    RETURN jsonb_array_length(missing);
END
$fn$;

DO $$
BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'navigator_app') THEN
        -- The conflict check's readers, and the loaders the runtime write
        -- path calls when it records one relationship or entity role.
        GRANT EXECUTE ON FUNCTION graph_conflict_frontier(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_conflict_edges(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_load_edges(text, text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_ensure_vertices(text, text) TO navigator_app;
        -- The authoritative graph-table rows, and the names the vertex
        -- payload carries. No UPDATE, DELETE, or TRUNCATE: graph rows are
        -- append-only from the runtime's seat, and the mirror's
        -- reconciliation is retired at this cutover.
        GRANT SELECT, INSERT ON TABLE entity_role TO navigator_app;
        GRANT SELECT, INSERT ON TABLE "relationship" TO navigator_app;
        GRANT SELECT ON TABLE person, entity, project, firm TO navigator_app;
    END IF;
END
$$;
