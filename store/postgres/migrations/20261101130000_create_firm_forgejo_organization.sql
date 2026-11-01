-- A Firm's organization on the installation's Forgejo, as Navigator's
-- provisioning identity last reconciled it. The organization is named by the
-- Firm's immutable slug, so it never moves; forgejo_id is the forge's own id,
-- which changes only when the organization is deleted and recreated.
-- owner_login is the account the Firm's own access token authenticates, kept
-- in the organization's owners team. One row per Firm: a re-run updates
-- reconciled_at and never inserts a second row.
CREATE TABLE firm_forgejo_organization (
    firm_id UUID PRIMARY KEY REFERENCES firm(id) ON DELETE CASCADE,
    organization TEXT NOT NULL UNIQUE
        CHECK (organization ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(organization) <= 40),
    forgejo_id BIGINT NOT NULL CHECK (forgejo_id > 0),
    owner_login TEXT NOT NULL CHECK (owner_login <> ''),
    provisioned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reconciled_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
