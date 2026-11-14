-- The runtime write path's two graph writers run as the graph's owner.
--
-- `cypher()` executes as the calling login, and the runtime login
-- (`navigator_app`) holds SELECT on the graph's label tables and nothing
-- more. Called as that login, `graph_load_edges` is refused INSERT on the
-- edge label table, and `graph_ensure_vertices` is refused EXECUTE on
-- `graph_load_vertices`, which stays revoked from it, so every runtime
-- relationship or entity-role write fails at its graph half.
--
-- SECURITY DEFINER runs the two functions as their owner, the migration
-- login that owns every graph object and every label created later. The
-- runtime login can then write exactly the fixed Cypher these bodies hold:
-- one edge between existing ids, and the missing endpoints of that edge.
-- Granting it INSERT on the label tables instead would let any `cypher()`
-- call it makes create arbitrary graph content, and every label added later
-- would need its own grant.
--
-- The pinned search path keeps the bodies' unqualified names resolving
-- where they did, with `pg_temp` last so no temporary object can shadow
-- one. EXECUTE stays revoked from PUBLIC and granted to `navigator_app`
-- alone (20261031025000).
DO $$
BEGIN
    ALTER FUNCTION graph_load_edges(text, text)
        SECURITY DEFINER SET search_path = public, ag_catalog, pg_temp;
    ALTER FUNCTION graph_ensure_vertices(text, text)
        SECURITY DEFINER SET search_path = public, ag_catalog, pg_temp;
END
$$;
