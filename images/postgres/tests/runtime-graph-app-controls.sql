\set ON_ERROR_STOP on
-- Actual navigator_app TCP/login, never superuser SET ROLE. Synthetic rows roll back.
BEGIN;
DO $$ BEGIN
 IF session_user <> 'navigator_app' OR current_user <> session_user OR EXISTS(
  SELECT 1 FROM pg_roles WHERE rolname=current_user AND (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb)
 ) THEN RAISE EXCEPTION 'actual restricted application login required'; END IF;
 IF EXISTS(SELECT 1 FROM pg_namespace WHERE nspname IN ('public','navigator_graph') AND has_schema_privilege(current_user,oid,'CREATE')) THEN
  RAISE EXCEPTION 'App has schema CREATE'; END IF;
 IF (SELECT count(*) FROM pg_proc p JOIN pg_roles r ON r.oid=p.proowner
  WHERE p.oid IN ('graph_ensure_vertices(text,text)'::regprocedure,'graph_load_edges(text,text)'::regprocedure)
  AND r.rolname='navigator_owner' AND NOT r.rolsuper AND NOT r.rolbypassrls AND p.prosecdef
  AND 'search_path=pg_catalog, public, ag_catalog, pg_temp'=ANY(p.proconfig)
  AND 'row_security=on'=ANY(p.proconfig))<>2 THEN
  RAISE EXCEPTION 'validated wrappers have wrong owner/configuration'; END IF;
END $$;
INSERT INTO person(id,name,email,role) VALUES
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','Synthetic graph first','graph-first@example.test','client'),
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2','Synthetic graph second','graph-second@example.test','client');
-- Equal UUIDs in distinct canonical tables must remain distinct graph vertices.
INSERT INTO entity_type(id,name) VALUES
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5ca','Synthetic graph Entity kind');
INSERT INTO jurisdiction(id,name) VALUES
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5cb','Synthetic graph jurisdiction');
INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','Synthetic graph Entity first',
  'a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5ca','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5cb'),
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2','Synthetic graph Entity second',
  'a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5ca','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5cb');
INSERT INTO relationship(id,confidence_pct,detail,inserted_at,kind,source_id,source_kind,updated_at,"in","out") VALUES
 ('synthetic-runtime-graph-edge',100,'Synthetic exact provenance',now(),'adverse_to',to_jsonb('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3'::text),'manual',now(),
 '{"tb":"person","id":{"Uuid":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1"}}',
 '{"tb":"person","id":{"Uuid":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2"}}');
DO $$
DECLARE payload jsonb; item jsonb; graph_before jsonb; n bigint; denied boolean; command text; label text;
BEGIN
 SELECT count(*) INTO n FROM navigator_graph."Person" WHERE properties::text::jsonb->>'id' IN
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2');
 IF n<>0 THEN RAISE EXCEPTION 'NEW vertex proof was preseeded'; END IF;
 PERFORM graph_ensure_vertices('person','{"rows":[{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1","name":"caller cannot forge name"},{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2"}]}');
 SELECT count(*) INTO n FROM navigator_graph."Person" WHERE properties::text::jsonb->>'id' IN
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2');
 IF n<>2 THEN RAISE EXCEPTION 'actual App did not create BOTH vertices'; END IF;
 IF NOT EXISTS(SELECT 1 FROM navigator_graph."Person" WHERE properties::text::jsonb @>
 '{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1","name":"Synthetic graph first"}') THEN
  RAISE EXCEPTION 'vertex properties were not canonically derived'; END IF;
 -- Project the Entity twins after Persons exist: neither label may satisfy the other.
 SELECT count(*) INTO n FROM navigator_graph."Entity" WHERE properties::text::jsonb->>'id' IN
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2');
 IF n<>0 THEN RAISE EXCEPTION 'NEW Entity proof was preseeded'; END IF;
 PERFORM graph_ensure_vertices('entity','{"rows":[{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1","name":"caller cannot forge Entity name"},{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2"}]}');
 SELECT count(*) INTO n FROM navigator_graph."Entity" WHERE properties::text::jsonb->>'id' IN
 ('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1','a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2');
 IF n<>2 THEN RAISE EXCEPTION 'actual App did not create BOTH Entity twins'; END IF;
 IF NOT EXISTS(SELECT 1 FROM navigator_graph."Entity" WHERE properties::text::jsonb @>
 '{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1","name":"Synthetic graph Entity first"}') THEN
  RAISE EXCEPTION 'Entity vertex properties were not canonically derived'; END IF;
 item:='{"from_id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1","to_id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2","from_kind":"person","to_kind":"person","kind":"adverse_to","confidence_pct":100,"source_kind":"manual","source_id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3","detail":"Synthetic exact provenance"}';
 -- A legitimate first effect followed by forged provenance must roll back the WHOLE statement.
 payload:=jsonb_build_object('rows',jsonb_build_array(item,jsonb_set(item,'{confidence_pct}','17')));
 denied:=false;
 BEGIN PERFORM * FROM graph_load_edges('relationship',payload::text);
 EXCEPTION WHEN check_violation THEN denied:=true; END;
 IF NOT denied THEN RAISE EXCEPTION 'forged provenance accepted'; END IF;
 IF EXISTS(SELECT 1 FROM navigator_graph."RELATIONSHIP" WHERE properties::text::jsonb->>'source_id'='a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3') THEN
  RAISE EXCEPTION 'failed multirow projection left a prefix edge'; END IF;
 -- A distinct forged source UUID must also refuse, not merely altered confidence.
 SELECT coalesce(jsonb_agg(to_jsonb(edge) ORDER BY edge.id),'[]'::jsonb)
 INTO graph_before FROM navigator_graph."RELATIONSHIP" edge;
 payload:=jsonb_build_object('rows',jsonb_build_array(item,
  jsonb_set(item,'{source_id}',to_jsonb('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c4'::text))));
 denied:=false;
 BEGIN PERFORM * FROM graph_load_edges('relationship',payload::text);
 EXCEPTION WHEN check_violation THEN denied:=true; END;
 IF NOT denied THEN RAISE EXCEPTION 'mismatched canonical source_id accepted'; END IF;
 IF graph_before IS DISTINCT FROM (
  SELECT coalesce(jsonb_agg(to_jsonb(edge) ORDER BY edge.id),'[]'::jsonb)
  FROM navigator_graph."RELATIONSHIP" edge
 ) THEN RAISE EXCEPTION 'forged source batch changed graph or left a valid prefix'; END IF;
 PERFORM * FROM graph_load_edges('relationship',jsonb_build_object('rows',jsonb_build_array(item))::text);
 SELECT count(*) INTO n FROM navigator_graph."RELATIONSHIP" WHERE properties::text::jsonb @>
 '{"source_id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3","detail":"Synthetic exact provenance","confidence_pct":100,"kind":"adverse_to"}';
 IF n<>1 THEN RAISE EXCEPTION 'actual NEW edge/provenance missing'; END IF;
 -- Physical graph endpoint IDs must name Person labels, despite equal property UUIDs.
 SELECT count(*) INTO n FROM navigator_graph."RELATIONSHIP" edge
 JOIN navigator_graph."Person" a ON a.id::text=edge.start_id::text
 JOIN navigator_graph."Person" z ON z.id::text=edge.end_id::text
 WHERE edge.properties::text::jsonb->>'source_id'='a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3'
 AND a.properties::text::jsonb->>'id'='a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1'
 AND z.properties::text::jsonb->>'id'='a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2';
 IF n<>1 THEN RAISE EXCEPTION 'canonical relationship did not retain Person to Person endpoints'; END IF;
 SELECT count(*) INTO n FROM navigator_graph."RELATIONSHIP" edge
 JOIN navigator_graph."Entity" entity ON entity.id::text=edge.start_id::text OR entity.id::text=edge.end_id::text
 WHERE edge.properties::text::jsonb->>'source_id'='a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c3';
 IF n<>0 THEN RAISE EXCEPTION 'equal UUID redirected canonical relationship to Entity'; END IF;
 denied:=false;
 BEGIN PERFORM graph_ensure_vertices('person','{"rows":[{"id":"a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5cf"}]}');
 EXCEPTION WHEN foreign_key_violation THEN denied:=true; END;
 IF NOT denied THEN RAISE EXCEPTION 'invented absent vertex accepted'; END IF;
 FOREACH label IN ARRAY ARRAY['Person','Entity','Project','Firm','HAS_ROLE','RELATIONSHIP','WORKS_ON','MEMBER_OF','HANDLES'] LOOP
  FOREACH command IN ARRAY ARRAY[
   format('INSERT INTO navigator_graph.%I SELECT * FROM navigator_graph.%I WHERE false',label,label),
   format('UPDATE navigator_graph.%I SET properties=properties WHERE false',label),
   format('DELETE FROM navigator_graph.%I WHERE false',label),format('TRUNCATE navigator_graph.%I',label)
  ] LOOP
   denied:=false; BEGIN EXECUTE command; EXCEPTION WHEN insufficient_privilege THEN denied:=true; END;
   IF NOT denied THEN RAISE EXCEPTION 'direct mutation allowed: %',command; END IF;
  END LOOP;
 END LOOP;
 FOREACH command IN ARRAY ARRAY[
  'SELECT * FROM graph_load_vertices(''person'',''{"rows":[]}'')',
  'SELECT * FROM graph_load_edges_raw(''relationship'',''{"rows":[]}'')',
  'SELECT graph_ensure_vertices_raw(''person'',''{"rows":[]}'')',
  'SELECT * FROM graph_clear_navigator()', 'SELECT * FROM ag_catalog.ag_graph','SELECT * FROM ag_catalog.ag_label',
  'SELECT graph_canonical_edge(''relationship'',''{}''::jsonb)',
  'SELECT graph_canonical_vertex(''person'',''{}''::jsonb)',
  'SELECT graph_require_unfiltered_sources()', 'SELECT * FROM graph_insert_canonical_edge(''relationship'',''{}''::jsonb)'
 ] LOOP
  denied:=false; BEGIN EXECUTE command; EXCEPTION WHEN insufficient_privilege THEN denied:=true; END;
  IF NOT denied THEN RAISE EXCEPTION 'private graph operation allowed: %',command; END IF;
 END LOOP;
END $$;
ROLLBACK;
