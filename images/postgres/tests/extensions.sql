\set ON_ERROR_STOP on
CREATE EXTENSION age;
CREATE EXTENSION vector;
CREATE EXTENSION citext;
CREATE EXTENSION pg_trgm;
LOAD 'age';
SET search_path = ag_catalog, public;
SELECT create_graph('smoke');
SELECT * FROM cypher('smoke', $$ CREATE (:Person {name:'synthetic'}) $$) AS (v agtype);
CREATE TABLE smoke_text(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), name citext UNIQUE, body text, embedding vector(3), inserted_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
INSERT INTO smoke_text(name,body,embedding) VALUES ('Example','durable workflow recovery','[1,0,0]'),('Other','graph search','[0,1,0]');
CREATE INDEX ON smoke_text USING gin(to_tsvector('english', body));
CREATE INDEX ON smoke_text USING hnsw(embedding vector_cosine_ops);
CREATE INDEX ON smoke_text USING gin(body gin_trgm_ops);
DO $$ BEGIN
 IF current_setting('server_version_num')::int <> 160015 THEN RAISE EXCEPTION 'server version'; END IF;
 IF (SELECT extversion FROM pg_extension WHERE extname='citext') <> '1.6' THEN RAISE EXCEPTION 'citext version'; END IF;
 IF (SELECT extversion FROM pg_extension WHERE extname='pg_trgm') <> '1.6' THEN RAISE EXCEPTION 'trigram version'; END IF;
 IF NOT (to_tsvector('english','durable workflow recovery') @@ phraseto_tsquery('english','workflow recovery')) THEN RAISE EXCEPTION 'phrase'; END IF;
 IF (SELECT count(*) FROM smoke_text WHERE name='EXAMPLE') <> 1 THEN RAISE EXCEPTION 'citext'; END IF;
 IF (SELECT count(*) FROM smoke_text WHERE to_tsvector('english',body) @@ plainto_tsquery('english','workflows')) <> 1 THEN RAISE EXCEPTION 'fts'; END IF;
 IF ts_rank(to_tsvector('english','workflow recovery'),plainto_tsquery('english','workflow')) <= 0 THEN RAISE EXCEPTION 'ranking'; END IF;
 IF similarity('workflow','workflw') <= 0 THEN RAISE EXCEPTION 'trigram'; END IF;
 IF (SELECT name FROM smoke_text ORDER BY embedding <=> '[1,0,0]' LIMIT 1) <> 'Example' THEN RAISE EXCEPTION 'vector'; END IF;
 IF (SELECT extversion FROM pg_extension WHERE extname='vector') <> '0.8.2' THEN RAISE EXCEPTION 'vector version'; END IF;
 IF (SELECT extversion FROM pg_extension WHERE extname='age') <> '1.6.0' THEN RAISE EXCEPTION 'age version'; END IF;
END $$;
SELECT extname,extversion FROM pg_extension ORDER BY extname;
