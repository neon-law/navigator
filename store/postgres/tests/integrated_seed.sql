CREATE EXTENSION age;
CREATE EXTENSION citext;
CREATE EXTENSION vector;
LOAD 'age';
SET search_path = ag_catalog, public;
SELECT create_graph('compatibility_graph');
SELECT * FROM cypher('compatibility_graph', $$
    CREATE (:Probe {name: 'synthetic', quantity: 7})
$$) AS (v agtype);
CREATE TABLE public.compatibility_probe (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    at TIMESTAMPTZ NOT NULL,
    body JSONB NOT NULL,
    name CITEXT NOT NULL,
    embedding VECTOR(3) NOT NULL,
    search TSVECTOR NOT NULL
);
INSERT INTO public.compatibility_probe(at, body, name, embedding, search)
VALUES ('2026-10-12T04:00:00-04:00', '{"synthetic":true}', 'Synthetic',
        '[1,2,2]', to_tsvector('english', 'synthetic document'));
CREATE TABLE public.effects(key TEXT PRIMARY KEY);
CREATE TABLE public.attempts(key TEXT NOT NULL);
