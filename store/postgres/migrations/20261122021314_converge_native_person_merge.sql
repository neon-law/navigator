-- This projection change is legal only after the relational merge has deleted
-- the dropped Person in the same uncommitted transaction. Existing row/actor
-- authorization remains at the Person command boundary.
CREATE FUNCTION graph_merge_person(kept UUID, dropped UUID) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public,ag_catalog,pg_temp SET row_security=on AS $fn$
DECLARE payload ag_catalog.agtype; item JSONB; edge_count BIGINT; name TEXT; identifier UUID;
BEGIN
    PERFORM public.graph_require_unfiltered_sources();
    IF kept IS NULL OR dropped IS NULL OR kept=dropped THEN
        RAISE EXCEPTION 'distinct Person identities required' USING ERRCODE='22023';
    END IF;
    FOR identifier IN SELECT unnest(ARRAY[kept,dropped]) ORDER BY 1 LOOP
        PERFORM pg_advisory_xact_lock(1312902736,hashtext(identifier::text));
    END LOOP;
    PERFORM pg_advisory_xact_lock(1312902737,1);
    SELECT p.name INTO STRICT name FROM public.person p WHERE p.id=kept FOR UPDATE;
    IF EXISTS(SELECT 1 FROM public.person WHERE id=dropped) OR
       EXISTS(SELECT 1 FROM public.entity_role WHERE person_id=dropped) OR
       EXISTS(SELECT 1 FROM public.person_project_role WHERE person_id=dropped) OR
       EXISTS(SELECT 1 FROM public.person_firm_role WHERE person_id=dropped) OR
       EXISTS(SELECT 1 FROM public.relationship r WHERE
           (r."in"->>'tb'='person' AND r."in"->'id'->>'Uuid'=dropped::text) OR
           (r."out"->>'tb'='person' AND r."out"->'id'->>'Uuid'=dropped::text)) THEN
        RAISE EXCEPTION 'relational Person merge is incomplete' USING ERRCODE='23514';
    END IF;
    payload := jsonb_build_object('keep',kept::text,'drop',dropped::text,'name',name)::text::ag_catalog.agtype;
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (p:Person)-[r:HAS_ROLE]->() WHERE p.id=$keep OR p.id=$drop DELETE r $Q$,payload) AS t(v ag_catalog.agtype);
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (a)-[r:RELATIONSHIP]->(b) WHERE (label(a)='Person' AND (a.id=$keep OR a.id=$drop)) OR (label(b)='Person' AND (b.id=$keep OR b.id=$drop)) DELETE r $Q$,payload) AS t(v ag_catalog.agtype);
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (p:Person)-[r:WORKS_ON]->() WHERE p.id=$keep OR p.id=$drop DELETE r $Q$,payload) AS t(v ag_catalog.agtype);
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (p:Person)-[r:MEMBER_OF]->() WHERE p.id=$keep OR p.id=$drop DELETE r $Q$,payload) AS t(v ag_catalog.agtype);
    -- DELETE (not DETACH DELETE) refuses any unexpected edge rather than losing it.
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (p:Person) WHERE p.id=$drop DELETE p $Q$,payload) AS t(v ag_catalog.agtype);
    PERFORM public.graph_ensure_vertices('person',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',kept)))::text);
    PERFORM v FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (p:Person) WHERE p.id=$keep SET p.name=$name $Q$,payload) AS t(v ag_catalog.agtype);
    FOR item IN SELECT jsonb_build_object('from_id',person_id::text,'to_id',entity_id::text,'role',role) FROM public.entity_role WHERE person_id=kept LOOP
        PERFORM public.graph_ensure_vertices('entity',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',item->>'to_id')))::text);
        PERFORM * FROM public.graph_load_edges('has_role',jsonb_build_object('rows',jsonb_build_array(item))::text);
    END LOOP;
    FOR item IN SELECT jsonb_build_object('from_id',r."in"->'id'->>'Uuid','to_id',r."out"->'id'->>'Uuid',
        'from_kind',r."in"->>'tb','to_kind',r."out"->>'tb','kind',r.kind,'confidence_pct',r.confidence_pct,
        'source_kind',r.source_kind,'source_id',(CASE WHEN jsonb_typeof(r.source_id)='string' THEN r.source_id#>>'{}' WHEN jsonb_typeof(r.source_id->'id'->'Uuid')='string' THEN r.source_id->'id'->>'Uuid' ELSE NULL END),'detail',r.detail)
        FROM public.relationship r WHERE
        (r."in"->>'tb'='person' AND r."in"->'id'->>'Uuid'=kept::text) OR
        (r."out"->>'tb'='person' AND r."out"->'id'->>'Uuid'=kept::text) LOOP
        PERFORM public.graph_ensure_vertices(item->>'from_kind',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',item->>'from_id')))::text);
        PERFORM public.graph_ensure_vertices(item->>'to_kind',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',item->>'to_id')))::text);
        PERFORM * FROM public.graph_load_edges('relationship',jsonb_build_object('rows',jsonb_build_array(item))::text);
    END LOOP;
    FOR item IN SELECT jsonb_build_object('from_id',person_id::text,'to_id',project_id::text,'role',participation) FROM public.person_project_role WHERE person_id=kept LOOP
        PERFORM public.graph_ensure_vertices('project',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',item->>'to_id')))::text);
        PERFORM * FROM public.graph_load_edges('works_on',jsonb_build_object('rows',jsonb_build_array(item))::text);
    END LOOP;
    FOR item IN SELECT jsonb_build_object('from_id',person_id::text,'to_id',firm_id::text) FROM public.person_firm_role WHERE person_id=kept LOOP
        PERFORM public.graph_ensure_vertices('firm',jsonb_build_object('rows',jsonb_build_array(jsonb_build_object('id',item->>'to_id')))::text);
        PERFORM * FROM public.graph_load_edges('member_of',jsonb_build_object('rows',jsonb_build_array(item))::text);
    END LOOP;
    FOR item IN SELECT DISTINCT jsonb_build_object('from_id',f.firm_id::text,'to_id',p.project_id::text)
        FROM public.person_firm_role f JOIN public.person_project_role p USING(person_id) WHERE f.person_id=kept LOOP
        payload := item::text::ag_catalog.agtype;
        SELECT count(*) INTO edge_count FROM ag_catalog.cypher('navigator_graph',$Q$ MATCH (f:Firm)-[r:HANDLES]->(p:Project) WHERE f.id=$from_id AND p.id=$to_id RETURN r $Q$,payload) AS t(v ag_catalog.agtype);
        IF edge_count=0 THEN
            PERFORM * FROM public.graph_load_edges('handles',jsonb_build_object('rows',jsonb_build_array(item))::text);
        END IF;
    END LOOP;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_merge_person(UUID,UUID) FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='navigator_app') THEN
        GRANT EXECUTE ON FUNCTION public.graph_merge_person(UUID,UUID) TO navigator_app;
    END IF;
END;
$$;
