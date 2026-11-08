-- The privileged PostgreSQL roles Job provisions PostGIS for the ordinary migrator.
-- A fresh schema installed by the bootstrap superuser can create it directly.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis') THEN
        CREATE EXTENSION postgis WITH SCHEMA public;
    END IF;
END
$$;
