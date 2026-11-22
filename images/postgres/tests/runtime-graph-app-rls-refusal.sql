\set ON_ERROR_STOP on
-- Run using actual navigator_app after owner enables relationship RLS on fresh owned fixture.
DO $$ BEGIN
 IF session_user<>'navigator_app' OR current_user<>session_user THEN RAISE EXCEPTION 'actual App login required'; END IF;
 IF NOT EXISTS(SELECT 1 FROM pg_class WHERE oid='public.relationship'::regclass AND relrowsecurity) THEN
  RAISE EXCEPTION 'real RLS control was not enabled'; END IF;
 BEGIN
  PERFORM graph_ensure_vertices('person','{"rows":[]}');
  RAISE EXCEPTION 'owner projector bypassed RLS';
 EXCEPTION WHEN insufficient_privilege THEN NULL; END;
 BEGIN
  PERFORM graph_merge_person('a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c1'::uuid,
                            'a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c2'::uuid);
  RAISE EXCEPTION 'merge projector bypassed RLS';
 EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END $$;
