LOAD 'age';
SET search_path = ag_catalog, public;
DO $$
BEGIN
    -- The image digest pins the exact matrix and moves every week with the
    -- base image's PostgreSQL minor, so this holds only what a weekly refresh
    -- must not change: the major, and each extension at its image's version.
    IF current_setting('server_version_num')::int / 10000 <> 16
       OR NOT EXISTS (
           SELECT 1 FROM pg_available_extensions WHERE name = 'age' AND installed_version = default_version
       )
       OR NOT EXISTS (
           SELECT 1 FROM pg_available_extensions WHERE name = 'vector' AND installed_version = default_version
       ) THEN
        RAISE EXCEPTION 'compatibility version matrix differs from its pin';
    END IF;
    IF (SELECT count(*) FROM public.project WHERE code = 'synthetic-test-project') <> 1 THEN
        RAISE EXCEPTION 'application Project did not survive recovery';
    END IF;
    IF (SELECT count(*) FROM public.social_publication WHERE state = 'confirmed') <> 1 THEN
        RAISE EXCEPTION 'application schema upgrade did not survive recovery';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM public.compatibility_probe
        WHERE at = '2026-10-12T08:00:00Z' AND body = '{"synthetic":true}'::jsonb
          AND name = 'synthetic'::citext AND embedding <-> '[0,0,0]'::vector = 3
          AND search @@ plainto_tsquery('english', 'synthetic document')
    ) THEN
        RAISE EXCEPTION 'SQL/extension values did not survive recovery';
    END IF;
    IF (SELECT count(*) FROM cypher('compatibility_graph', $cypher$
        MATCH (n:Probe) WHERE n.name = 'synthetic' AND n.quantity = 7 RETURN n
    $cypher$) AS (v agtype)) <> 1 THEN
        RAISE EXCEPTION 'AGE traversal did not survive recovery';
    END IF;
END $$;
