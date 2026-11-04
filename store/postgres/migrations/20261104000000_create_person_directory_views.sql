-- The people directory (`/app/admin/people`) read as two views, so the page
-- is one indexed query instead of a membership lookup per Person.
--
-- `person_directory` is each Person with the Firms they hold a
-- `person_firm_role` on, names in membership order. `firm_visible_person`
-- is who an Admin of a Firm may see: its members, plus anyone with a
-- `person_project_role` on a Project that Firm owns.
--
-- Plain views, not materialized: they read the live rows, so a write is
-- visible on the next page load with no refresh to schedule. Runtime SELECT
-- comes from the `navigator_owner` default privileges in roles.sql.

CREATE VIEW person_directory AS
SELECT p.id, p.name, p.given_name, p.family_name, p.middle_name, p.email, p.email_lower, p.oidc_subject, p.microsoft_subject, p.apple_subject, p.role, p.title, p.phone, p.xero_contact_id, p.profile_image_url, p.linkedin_url, p.email_confirmed, p.is_admitted, p.inserted_at, p.updated_at,
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

CREATE VIEW firm_visible_person AS
SELECT r.firm_id, r.person_id FROM person_firm_role r
UNION
SELECT pr.firm_id, ppr.person_id
FROM person_project_role ppr
JOIN project pr ON pr.id = ppr.project_id
WHERE pr.firm_id IS NOT NULL;

-- The lateral join's per-Person lookup already rides the
-- `UNIQUE (person_id, firm_id)` index. This covers the Firm-to-Project hop of `firm_visible_person`.
CREATE INDEX project_firm ON project (firm_id);
