-- Migration: 20261031020000_create_graph_view_readers.sql
-- Read-only reader functions for the graph views: bounded neighborhood
-- expansion (project, lawyer) and the firm-to-firm aggregation. These are
-- the Cypher entry points for the graph REST endpoints, the same pattern
-- as the conflict readers in 20261031010000_create_navigator_graph.sql:
-- each helper takes one JSON text payload, parses it into an agtype once,
-- and passes it as a PL/pgSQL variable. The Cypher in each branch is a
-- fixed dollar-quoted constant: no value ever becomes query text.
--
-- AGE 1.6.0 constraints honored here (docs/age-cypher-parity-probe.md):
-- no multi-type edge patterns — every label is matched in its own query,
-- hop by hop; no order-by on a Cypher alias (ordering happens SQL-side or
-- in Rust); `id(e)`/`label(n)` rather than property access.

-- The seed vertex for a view: id, label, and name. An absent vertex means
-- the graph projection has not been run yet — callers render an empty
-- neighborhood, not an error.
CREATE OR REPLACE FUNCTION graph_view_seed(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)
        WHERE n.id = $seed
        RETURN DISTINCT {id: n.id, label: label(n), name: n.name}
    $Q$, payload) AS (vertex agtype);
END
$fn$;

-- Hop 1 of the project view: everything hanging directly off the project
-- vertex — the parties (WORKS_ON, undirected) and the firms whose members
-- participate on it (HANDLES, directed at the project).
CREATE OR REPLACE FUNCTION graph_project_attach(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)-[r:WORKS_ON]-(p:Project)
        WHERE p.id = $seed
        RETURN DISTINCT {edge_id: id(r), edge_type: 'WORKS_ON', kind: null, role: r.role,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)-[r:HANDLES]->(p:Project)
        WHERE p.id = $seed
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HANDLES', kind: null, role: null,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- Hops 2+ of the project view: one frontier step outward from the already
-- reached persons, entities, and firms. Only persons expand — relationships
-- and entity roles anchor on persons, and a firm's participation is already
-- materialized as its HANDLES edges. Because each hop passes its own
-- frontier, no vertex is expanded twice (same bounded walk as the conflict
-- readers).
CREATE OR REPLACE FUNCTION graph_project_expand(frontier text)
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
                         confidence_pct: r.confidence_pct, source_kind: r.source_kind,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:MEMBER_OF]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'MEMBER_OF', kind: null, role: null,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- Every hop of the lawyer view: everything around the lawyer-person seed
-- and then one frontier step outward. Persons expand through their
-- relationships, entity roles, memberships, and participations; reached
-- projects and firms are endpoint-only (only persons expand — the project
-- view owns project-rooted neighborhoods).
CREATE OR REPLACE FUNCTION graph_lawyer_expand(frontier text)
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
                         confidence_pct: r.confidence_pct, source_kind: r.source_kind,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:MEMBER_OF]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'MEMBER_OF', kind: null, role: null,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:WORKS_ON]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'WORKS_ON', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- The firm view: person-level edges aggregated up to firm-firm ties for the
-- seed firm. Every counterpart firm in a person's edge sits three hops away
-- (member -> person-level edge -> member's firm), so this is one aggregation
-- read, not a frontier walk — depth-independent by construction.
--
-- kind values mirror the lens a viewer reads: adversary (an adverse_to
-- relationship between members), co_counsel (members firm-side on the same
-- matter), of_counsel (any other cross-firm relationship between members),
-- client (a counterparty-firm member client-side on a matter this firm's
-- members staff firm-side). strength counts the distinct person-level edges
-- behind the tie.
CREATE OR REPLACE FUNCTION graph_firm_view(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    -- Each arm returns plain grouping columns (AGE rejects a RETURN map that
    -- mixes grouping keys with aggregates) and the SELECT wraps them into one
    -- JSON map per tie. The `(x::text)::jsonb` round-trip re-homes an agtype
    -- value into the JSON object verbatim.
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'adversary',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:RELATIONSHIP]-(b:Person)
        WHERE r.kind = 'adverse_to'
        MATCH (b)-[:MEMBER_OF]->(fb:Firm)
        WHERE fb.id <> fa.id
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT r) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'co_counsel',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:WORKS_ON]->(p:Project)
        WHERE r.role <> 'client'
        MATCH (m2:Person)-[r2:WORKS_ON]->(p)
        MATCH (m2)-[:MEMBER_OF]->(fb:Firm)
        WHERE m2 <> a AND fb.id <> fa.id AND r2.role <> 'client'
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT p) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'of_counsel',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:RELATIONSHIP]-(b:Person)
        WHERE r.kind <> 'adverse_to'
        MATCH (b)-[:MEMBER_OF]->(fb:Firm)
        WHERE fb.id <> fa.id
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT r) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'client',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:WORKS_ON]->(p:Project)
        WHERE r.role <> 'client'
        MATCH (c:Person)-[r2:WORKS_ON]->(p)
        MATCH (c)-[:MEMBER_OF]->(fb:Firm)
        WHERE c <> a AND fb.id <> fa.id AND r2.role = 'client'
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT c) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
END
$fn$;

-- View readers are the runtime login's only graph entry points, like the
-- conflict readers before them: revoking PUBLIC and granting navigator_app
-- when the role exists (guarded, because role-less CI fixtures skip it).
REVOKE EXECUTE ON FUNCTION graph_view_seed(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_project_attach(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_project_expand(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_lawyer_expand(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_firm_view(text) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        GRANT EXECUTE ON FUNCTION graph_view_seed(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_project_attach(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_project_expand(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_lawyer_expand(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_firm_view(text) TO navigator_app;
    END IF;
END
$$;
