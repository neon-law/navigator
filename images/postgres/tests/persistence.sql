\set ON_ERROR_STOP on
LOAD 'age';
SET search_path = ag_catalog, public;
DO $$ BEGIN
 IF (SELECT count(*) FROM smoke_text) <> 2 THEN RAISE EXCEPTION 'lost rows'; END IF;
 IF (SELECT count(*) FROM cypher('smoke', $cypher$ MATCH (p:Person) RETURN p $cypher$) AS (p agtype)) <> 1 THEN RAISE EXCEPTION 'lost graph'; END IF;
END $$;
SELECT * FROM cypher('smoke', $$ MATCH (p:Person) RETURN p.name $$) AS (name agtype);
