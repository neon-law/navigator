-- Synthetic rows exercise the initial target slice's types, FKs, and timestamps.
INSERT INTO jurisdiction (name, code, jurisdiction_type)
VALUES ('Synthetic Test State', 'ST', 'state');

INSERT INTO entity_type (name)
VALUES ('Synthetic Test Organization');

INSERT INTO entity (name, entity_type_id, jurisdiction_id)
SELECT 'Synthetic Test Entity', entity_type.id, jurisdiction.id
FROM entity_type, jurisdiction
WHERE entity_type.name = 'Synthetic Test Organization'
  AND jurisdiction.code = 'ST';

INSERT INTO firm (name, slug, status)
VALUES ('Synthetic Test Firm', 'synthetic-test-firm', 'active');

INSERT INTO brand (name, brand_key, firm_id)
SELECT 'Synthetic Test Brand', 'synthetic-test', firm.id
FROM firm
WHERE firm.name = 'Synthetic Test Firm';

INSERT INTO project (code, name, status, entity_id, firm_id, brand)
SELECT 'synthetic-test-project', 'Synthetic Test Project', 'open', entity.id, firm.id, 'synthetic-test'
FROM entity, firm
WHERE entity.name = 'Synthetic Test Entity'
  AND firm.name = 'Synthetic Test Firm';

INSERT INTO person (name, email)
VALUES ('Synthetic Test Person', 'synthetic@example.invalid');

INSERT INTO asset (storage_key, content_type, byte_size, sha256_hex, project_id)
SELECT 'synthetic/test/document.pdf', 'application/pdf', 0, repeat('0', 64), project.id
FROM project
WHERE project.code = 'synthetic-test-project';

INSERT INTO relationship_log (subject_type, subject_id, action, detail, project_id)
SELECT 'project', project.id, 'created', 'Synthetic relationship log fixture', project.id
FROM project
WHERE project.code = 'synthetic-test-project';

DO $$
BEGIN
    IF (SELECT count(*) FROM project WHERE code = 'synthetic-test-project') <> 1 THEN
        RAISE EXCEPTION 'synthetic project seed was not inserted';
    END IF;
    IF (SELECT count(*) FROM asset WHERE project_id = (SELECT id FROM project WHERE code = 'synthetic-test-project')) <> 1 THEN
        RAISE EXCEPTION 'Project-scoped synthetic Asset was not inserted';
    END IF;
    IF (SELECT count(*) FROM relationship_log WHERE project_id = (SELECT id FROM project WHERE code = 'synthetic-test-project')) <> 1 THEN
        RAISE EXCEPTION 'Project-scoped synthetic Relationship Log was not inserted';
    END IF;
    IF (SELECT id::text FROM person WHERE email = 'synthetic@example.invalid') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' THEN
        RAISE EXCEPTION 'generated Person id is not UUIDv4';
    END IF;
    IF (SELECT inserted_at FROM person WHERE email = 'synthetic@example.invalid') IS NULL
       OR (SELECT updated_at FROM person WHERE email = 'synthetic@example.invalid') IS NULL THEN
        RAISE EXCEPTION 'timestamp columns were not set';
    END IF;
END $$;

-- A Post, the handle its Brand holds on X, and one confirmed publication of the
-- Post to X. The unique (post_id, provider) pair makes a repeated send idempotent.
INSERT INTO post (brand_key, slug, title, description, body_markdown, status, publish_at)
VALUES (
    'synthetic-test', 'synthetic-post', 'Synthetic Post', 'A synthetic Post.',
    'Synthetic body.', 'published', '2026-01-02T00:00:00Z'
);

INSERT INTO social_handle (brand_key, provider, handle)
VALUES ('synthetic-test', 'x', 'SyntheticHandle');

INSERT INTO social_publication (post_id, provider, state, attempt_count, provider_post_id, permalink)
SELECT post.id, 'x', 'confirmed', 1, '1', 'https://x.com/SyntheticHandle/status/1'
FROM post
WHERE post.brand_key = 'synthetic-test' AND post.slug = 'synthetic-post';
