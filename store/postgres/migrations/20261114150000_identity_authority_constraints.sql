-- Uniqueness PostgreSQL enforces itself once People authority writes identity
-- detail. SurrealDB holds the same rules as UNIQUE indexes, so mirrored rows
-- already satisfy them.

-- One Entity per Firm anchor key: the claim a second anchor collides with.
CREATE UNIQUE INDEX entity_firm_anchor_key ON entity (firm_anchor_key)
    WHERE firm_anchor_key IS NOT NULL;

-- One row per number per Person.
CREATE UNIQUE INDEX person_phone_pair ON person_phone (person_id, e164);
