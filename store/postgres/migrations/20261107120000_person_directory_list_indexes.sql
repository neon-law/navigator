-- The People directory reads one keyset page at a time (store::listing):
-- each sort walks a `(field, id)` index from the cursor, and the substring
-- filters (`ILIKE '%…%'`) ride trigram indexes instead of scanning every row.
--
-- `pg_trgm` is a trusted extension, so the migration login (the database
-- owner) may create it; `k8s/postgres/access/roles.sql` also creates it in
-- the superuser init path. The bootstrap is a DO block because the
-- canonical-SQL parser treats procedural blocks as opaque.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm') THEN
        CREATE EXTENSION pg_trgm;
    END IF;
END
$$;

CREATE INDEX person_name_id ON person (name, id);
CREATE INDEX person_email_id ON person (email, id);
CREATE INDEX person_name_trgm ON person USING gin (name gin_trgm_ops);
CREATE INDEX person_email_trgm ON person USING gin ((email::text) gin_trgm_ops);
