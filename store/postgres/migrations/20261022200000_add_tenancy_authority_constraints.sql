-- Keep PostgreSQL's write authority within the invariants already enforced by
-- SurrealDB. Index creation is intentionally fail-closed if imported rows
-- contain collisions; operators must resolve those rows before cutover.
DO $$
DECLARE
    firm_entity_groups BIGINT;
    brand_name_groups BIGINT;
    invalid_font_licences BIGINT;
BEGIN
    SELECT count(*) INTO firm_entity_groups FROM (
        SELECT entity_id FROM firm WHERE entity_id IS NOT NULL
        GROUP BY entity_id HAVING count(*) > 1
    ) collisions;
    SELECT count(*) INTO brand_name_groups FROM (
        SELECT name FROM brand GROUP BY name HAVING count(*) > 1
    ) collisions;
    SELECT count(*) INTO invalid_font_licences FROM brand
        WHERE font_licence IS NOT NULL AND font_licence NOT IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0');
    IF firm_entity_groups > 0 OR brand_name_groups > 0 OR invalid_font_licences > 0 THEN
        RAISE EXCEPTION 'tenancy constraint preflight failed: firm entity duplicate groups %, brand name duplicate groups %, invalid font licence rows %',
            firm_entity_groups, brand_name_groups, invalid_font_licences;
    END IF;
END $$;
CREATE UNIQUE INDEX firm_entity ON firm (entity_id) WHERE entity_id IS NOT NULL;
CREATE UNIQUE INDEX brand_name ON brand (name);
ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));
