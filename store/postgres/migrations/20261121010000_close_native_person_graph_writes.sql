-- Runtime graph maintenance derives every property from authoritative rows.
-- Raw Cypher loaders remain available only to their migration owner.
DO $$
BEGIN
    ALTER FUNCTION public.graph_load_edges(text,text) RENAME TO graph_load_edges_raw;
    ALTER FUNCTION public.graph_ensure_vertices(text,text) RENAME TO graph_ensure_vertices_raw;
    REVOKE ALL ON FUNCTION public.graph_load_edges_raw(text,text), public.graph_ensure_vertices_raw(text,text),
        public.graph_load_vertices(text,text), public.graph_clear_navigator() FROM PUBLIC;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='navigator_app') THEN
        REVOKE ALL ON FUNCTION public.graph_load_edges_raw(text,text), public.graph_ensure_vertices_raw(text,text), public.graph_clear_navigator() FROM navigator_app;
    END IF;
END;
$$;

-- A privileged projection must never bypass caller-visible row policies.
-- Current canonical sources have no RLS; a future policy needs an explicit
-- compatible projector rather than implicitly reading around that policy.
CREATE FUNCTION graph_require_unfiltered_sources() RETURNS VOID
LANGUAGE plpgsql SET search_path=pg_catalog AS $fn$
BEGIN
    IF EXISTS(SELECT 1 FROM pg_catalog.pg_class WHERE oid=ANY(ARRAY[
        'public.person'::regclass,'public.entity'::regclass,'public.project'::regclass,'public.firm'::regclass,
        'public.entity_role'::regclass,'public.relationship'::regclass,
        'public.person_project_role'::regclass,'public.person_firm_role'::regclass])
        AND (relrowsecurity OR relforcerowsecurity)) THEN
        RAISE EXCEPTION 'canonical graph row policies require an explicit compatible projector' USING ERRCODE='42501';
    END IF;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_require_unfiltered_sources() FROM PUBLIC;

CREATE FUNCTION graph_canonical_edge(kind TEXT, requested JSONB) RETURNS JSONB
LANGUAGE plpgsql SET search_path=pg_catalog,public,ag_catalog AS $fn$
DECLARE actual JSONB;
BEGIN
    IF kind='has_role' THEN
        SELECT jsonb_build_object('from_id',person_id::text,'to_id',entity_id::text,'role',role)
        INTO actual FROM public.entity_role
        WHERE person_id=(requested->>'from_id')::uuid AND entity_id=(requested->>'to_id')::uuid
          AND role=requested->>'role' LIMIT 1;
    ELSIF kind='relationship' THEN
        SELECT jsonb_build_object('from_id',r."in"->'id'->>'Uuid','to_id',r."out"->'id'->>'Uuid',
            'from_label',CASE r."in"->>'tb' WHEN 'person' THEN 'Person' ELSE 'Entity' END,
            'to_label',CASE r."out"->>'tb' WHEN 'person' THEN 'Person' ELSE 'Entity' END,
            'kind',r.kind,'confidence_pct',r.confidence_pct,'source_kind',r.source_kind,
            'source_id',(CASE WHEN jsonb_typeof(r.source_id)='string' THEN r.source_id#>>'{}' WHEN jsonb_typeof(r.source_id->'id'->'Uuid')='string' THEN r.source_id->'id'->>'Uuid' ELSE NULL END),'detail',r.detail)
        INTO actual FROM public.relationship r
        WHERE r."in"->>'tb' IN ('person','entity') AND r."out"->>'tb' IN ('person','entity')
          AND r."in"->>'tb'=requested->>'from_kind' AND r."out"->>'tb'=requested->>'to_kind'
          AND r."in"->'id'->>'Uuid'=requested->>'from_id' AND r."out"->'id'->>'Uuid'=requested->>'to_id'
          AND r.kind=requested->>'kind' AND to_jsonb(r.confidence_pct)=requested->'confidence_pct'
          AND r.source_kind=requested->>'source_kind'
          AND (CASE WHEN jsonb_typeof(r.source_id)='string' THEN r.source_id#>>'{}' WHEN jsonb_typeof(r.source_id->'id'->'Uuid')='string' THEN r.source_id->'id'->>'Uuid' ELSE NULL END) IS NOT DISTINCT FROM requested->>'source_id'
          AND r.detail IS NOT DISTINCT FROM requested->>'detail' LIMIT 1;
    ELSIF kind='works_on' THEN
        SELECT jsonb_build_object('from_id',person_id::text,'to_id',project_id::text,'role',participation)
        INTO actual FROM public.person_project_role
        WHERE person_id=(requested->>'from_id')::uuid AND project_id=(requested->>'to_id')::uuid
          AND participation=requested->>'role' LIMIT 1;
    ELSIF kind='member_of' THEN
        SELECT jsonb_build_object('from_id',person_id::text,'to_id',firm_id::text)
        INTO actual FROM public.person_firm_role
        WHERE person_id=(requested->>'from_id')::uuid AND firm_id=(requested->>'to_id')::uuid LIMIT 1;
    ELSIF kind='handles' THEN
        SELECT jsonb_build_object('from_id',f.firm_id::text,'to_id',p.project_id::text)
        INTO actual FROM public.person_firm_role f JOIN public.person_project_role p USING(person_id)
        WHERE f.firm_id=(requested->>'from_id')::uuid AND p.project_id=(requested->>'to_id')::uuid LIMIT 1;
    ELSE
        RAISE EXCEPTION 'unsupported canonical graph edge' USING ERRCODE='22023';
    END IF;
    IF actual IS NULL THEN
        RAISE EXCEPTION 'graph edge has no matching authoritative row' USING ERRCODE='23514';
    END IF;
    RETURN actual;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_canonical_edge(TEXT,JSONB) FROM PUBLIC;

CREATE FUNCTION graph_canonical_vertex(vertex_kind TEXT, requested JSONB) RETURNS JSONB
LANGUAGE plpgsql SET search_path=pg_catalog,public,ag_catalog AS $fn$
DECLARE identifier UUID := (requested->>'id')::uuid; display_name TEXT; present BOOLEAN;
BEGIN
    IF vertex_kind='person' THEN SELECT name INTO display_name FROM public.person WHERE id=identifier;
    ELSIF vertex_kind='entity' THEN SELECT name INTO display_name FROM public.entity WHERE id=identifier;
    ELSIF vertex_kind='project' THEN SELECT name INTO display_name FROM public.project WHERE id=identifier;
    ELSIF vertex_kind='firm' THEN SELECT name INTO display_name FROM public.firm WHERE id=identifier;
    ELSE RAISE EXCEPTION 'unsupported canonical graph vertex' USING ERRCODE='22023';
    END IF;
    present := FOUND;
    -- Historical orphan links retain a nameless vertex only while an actual
    -- canonical graph row still names them. No caller can invent that evidence.
    IF NOT present AND NOT (
        EXISTS(SELECT 1 FROM public.relationship r WHERE
            (r."in"->>'tb'=vertex_kind AND r."in"->'id'->>'Uuid'=identifier::text) OR
            (r."out"->>'tb'=vertex_kind AND r."out"->'id'->>'Uuid'=identifier::text)) OR
        EXISTS(SELECT 1 FROM public.entity_role WHERE
            (vertex_kind='person' AND person_id=identifier) OR (vertex_kind='entity' AND entity_id=identifier)) OR
        EXISTS(SELECT 1 FROM public.person_project_role WHERE
            (vertex_kind='person' AND person_id=identifier) OR (vertex_kind='project' AND project_id=identifier)) OR
        EXISTS(SELECT 1 FROM public.person_firm_role WHERE
            (vertex_kind='person' AND person_id=identifier) OR (vertex_kind='firm' AND firm_id=identifier))
    ) THEN
        RAISE EXCEPTION 'graph vertex has no authoritative row or retained link' USING ERRCODE='23503';
    END IF;
    RETURN jsonb_build_object('id',identifier::text,'name',display_name);
END;
$fn$;
REVOKE ALL ON FUNCTION graph_canonical_vertex(TEXT,JSONB) FROM PUBLIC;

CREATE FUNCTION graph_ensure_vertices(kind TEXT, rows TEXT) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public,ag_catalog,pg_temp SET row_security=on AS $fn$
DECLARE requested JSONB; canonical JSONB; payload ag_catalog.agtype; label_name TEXT; matches BIGINT; created INTEGER := 0;
BEGIN
    PERFORM public.graph_require_unfiltered_sources();
    label_name := CASE kind WHEN 'person' THEN 'Person' WHEN 'entity' THEN 'Entity'
        WHEN 'project' THEN 'Project' WHEN 'firm' THEN 'Firm' ELSE NULL END;
    IF label_name IS NULL THEN
        RAISE EXCEPTION 'unsupported canonical graph kind' USING ERRCODE='22023';
    END IF;
    -- All runtime graph writers share the same transaction-scoped projection lock.
    PERFORM pg_advisory_xact_lock(1312902737,1);
    FOR requested IN SELECT * FROM jsonb_array_elements(rows::jsonb->'rows') LOOP
        canonical := public.graph_canonical_vertex(kind,requested);
        payload := jsonb_build_object('id',canonical->>'id','label',label_name)::text::ag_catalog.agtype;
        SELECT count(*) INTO matches FROM ag_catalog.cypher('navigator_graph',$Q$
            MATCH (v) WHERE v.id=$id AND label(v)=$label RETURN v
        $Q$,payload) AS t(v ag_catalog.agtype);
        IF matches=0 THEN
            PERFORM * FROM public.graph_load_vertices(kind,jsonb_build_object('rows',jsonb_build_array(canonical))::text);
            created := created+1;
        END IF;
    END LOOP;
    RETURN created;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_ensure_vertices(TEXT,TEXT) FROM PUBLIC;

-- Fixed typed endpoints prevent equal UUIDs in different tables from crossing.
CREATE FUNCTION graph_insert_canonical_edge(kind TEXT, item JSONB) RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql SET search_path=pg_catalog,public,ag_catalog AS $fn$
DECLARE payload ag_catalog.agtype := jsonb_build_object('rows',jsonb_build_array(item))::text::ag_catalog.agtype;
BEGIN
    IF kind='has_role' THEN
        RETURN QUERY SELECT * FROM ag_catalog.cypher('navigator_graph',$Q$
            UNWIND $rows AS row MATCH (a:Person),(b:Entity)
            WHERE a.id=row.from_id AND b.id=row.to_id
            CREATE (a)-[:HAS_ROLE {role:row.role}]->(b)
        $Q$,payload) AS t(v ag_catalog.agtype);
    ELSIF kind='relationship' THEN
        RETURN QUERY SELECT * FROM ag_catalog.cypher('navigator_graph',$Q$
            UNWIND $rows AS row MATCH (a),(b)
            WHERE a.id=row.from_id AND label(a)=row.from_label AND b.id=row.to_id AND label(b)=row.to_label
            CREATE (a)-[:RELATIONSHIP {kind:row.kind,confidence_pct:row.confidence_pct,
                source_kind:row.source_kind,source_id:row.source_id,detail:row.detail}]->(b)
        $Q$,payload) AS t(v ag_catalog.agtype);
    ELSIF kind='works_on' THEN
        RETURN QUERY SELECT * FROM ag_catalog.cypher('navigator_graph',$Q$
            UNWIND $rows AS row MATCH (a:Person),(b:Project)
            WHERE a.id=row.from_id AND b.id=row.to_id CREATE (a)-[:WORKS_ON {role:row.role}]->(b)
        $Q$,payload) AS t(v ag_catalog.agtype);
    ELSIF kind='member_of' THEN
        RETURN QUERY SELECT * FROM ag_catalog.cypher('navigator_graph',$Q$
            UNWIND $rows AS row MATCH (a:Person),(b:Firm)
            WHERE a.id=row.from_id AND b.id=row.to_id CREATE (a)-[:MEMBER_OF]->(b)
        $Q$,payload) AS t(v ag_catalog.agtype);
    ELSIF kind='handles' THEN
        RETURN QUERY SELECT * FROM ag_catalog.cypher('navigator_graph',$Q$
            UNWIND $rows AS row MATCH (a:Firm),(b:Project)
            WHERE a.id=row.from_id AND b.id=row.to_id CREATE (a)-[:HANDLES]->(b)
        $Q$,payload) AS t(v ag_catalog.agtype);
    ELSE
        RAISE EXCEPTION 'unsupported canonical graph kind' USING ERRCODE='22023';
    END IF;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_insert_canonical_edge(TEXT,JSONB) FROM PUBLIC;

CREATE FUNCTION graph_load_edges(kind TEXT, rows TEXT) RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public,ag_catalog,pg_temp SET row_security=on AS $fn$
DECLARE requested JSONB; canonical JSONB;
BEGIN
    PERFORM public.graph_require_unfiltered_sources();
    IF kind IS NULL OR kind NOT IN ('has_role','relationship','works_on','member_of','handles') THEN
        RAISE EXCEPTION 'unsupported canonical graph kind' USING ERRCODE='22023';
    END IF;
    PERFORM pg_advisory_xact_lock(1312902737,1);
    FOR requested IN SELECT * FROM jsonb_array_elements(rows::jsonb->'rows') LOOP
        canonical := public.graph_canonical_edge(kind,requested);
        RETURN QUERY SELECT * FROM public.graph_insert_canonical_edge(kind,canonical);
    END LOOP;
END;
$fn$;
REVOKE ALL ON FUNCTION graph_load_edges(TEXT,TEXT) FROM PUBLIC;

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
    IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='navigator_app') THEN
        REVOKE ALL ON FUNCTION public.graph_load_vertices(TEXT,TEXT), public.graph_clear_navigator(),
            public.graph_canonical_edge(TEXT,JSONB), public.graph_canonical_vertex(TEXT,JSONB),
            public.graph_require_unfiltered_sources(), public.graph_insert_canonical_edge(TEXT,JSONB),
            public.graph_load_edges_raw(TEXT,TEXT), public.graph_ensure_vertices_raw(TEXT,TEXT) FROM navigator_app;
        GRANT EXECUTE ON FUNCTION public.graph_ensure_vertices(TEXT,TEXT), public.graph_load_edges(TEXT,TEXT), public.graph_merge_person(UUID,UUID) TO navigator_app;
    END IF;
END;
$$;
