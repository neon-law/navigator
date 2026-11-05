-- A Person's email is `citext`: equality and the unique index ignore letter
-- case, and the column keeps the casing it was entered with. The generated
-- `email_lower` twin and its unique constraint are redundant with that and
-- are dropped; `person_email_key` is the one uniqueness rule.
--
-- `citext` is a trusted extension, so the migration login (the database
-- owner) may create it; `k8s/postgres/access/roles.sql` also creates it in
-- the superuser init path. The bootstrap is a DO block because the
-- canonical-SQL parser treats procedural blocks as opaque.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'citext') THEN
        CREATE EXTENSION citext;
    END IF;
END
$$;

-- The directory view selects `email_lower`; it is recreated below.
DROP VIEW person_directory;
ALTER TABLE person DROP CONSTRAINT person_email_lower_key;
ALTER TABLE person DROP COLUMN email_lower;
ALTER TABLE person ALTER COLUMN email TYPE CITEXT;
CREATE UNIQUE INDEX person_email_key ON person (email);

CREATE VIEW person_directory AS
SELECT p.id, p.name, p.given_name, p.family_name, p.middle_name, p.email, p.oidc_subject, p.microsoft_subject, p.apple_subject, p.role, p.title, p.phone, p.xero_contact_id, p.profile_image_url, p.linkedin_url, p.email_confirmed, p.is_admitted, p.inserted_at, p.updated_at,
    COALESCE(m.firm_ids, '{}') AS firm_ids,
    COALESCE(m.firm_names, '{}') AS firm_names
FROM person p
LEFT JOIN LATERAL (
    SELECT array_agg(f.id ORDER BY r.inserted_at, r.id) AS firm_ids,
        array_agg(f.name ORDER BY r.inserted_at, r.id) AS firm_names
    FROM person_firm_role r
    JOIN firm f ON f.id = r.firm_id
    WHERE r.person_id = p.id
) m ON true;
