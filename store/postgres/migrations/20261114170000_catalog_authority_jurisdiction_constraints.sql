-- Under the catalog authority a Jurisdiction's type stays in the closed set the
-- SurrealDB schema asserts.
ALTER TABLE jurisdiction ADD CONSTRAINT jurisdiction_type_closed CHECK (jurisdiction_type IN ('state', 'country', 'global'));
