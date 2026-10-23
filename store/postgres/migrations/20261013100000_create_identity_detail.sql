-- What a Person or an Entity holds beside its own row: postal addresses,
-- licensure, single-use email tokens, structural ties to an Entity,
-- recorded delegations, accounts on external systems, and the mailbox a
-- Person's email claims. Columns keep the Surreal source names so the
-- identity-detail mirror and the parity manifest compare them one to one.
--
-- No foreign key cascades. SurrealDB never cascades a Person or Entity
-- delete to these rows, so a child whose parent is gone is drift for
-- parity to report, not a row for PostgreSQL to remove on its own.

CREATE TABLE address (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID REFERENCES person(id),
    entity_id UUID REFERENCES entity(id),
    line1 TEXT NOT NULL,
    line2 TEXT,
    city TEXT NOT NULL,
    region TEXT NOT NULL,
    postal_code TEXT NOT NULL,
    country TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A licence number is a public bar or licensing-authority number.
CREATE TABLE credential (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    jurisdiction_id UUID NOT NULL REFERENCES jurisdiction(id),
    license_number TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, jurisdiction_id)
);

-- Only the SHA-256 hex of a token is stored; its plaintext never is.
CREATE TABLE email_token (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    email TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('password_reset', 'email_confirm')),
    token_hash TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- SurrealDB stores this tie as a relation edge whose `in` is the Person and
-- whose `out` is the Entity. Its identity is the Person, Entity, and role.
CREATE TABLE entity_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    entity_id UUID NOT NULL REFERENCES entity(id),
    role TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, entity_id, role)
);

-- A revoked delegation stays on the table, so a pair may hold several rows.
CREATE TABLE person_delegate (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    delegate_person_id UUID NOT NULL REFERENCES person(id),
    subject_person_id UUID NOT NULL REFERENCES person(id),
    basis TEXT NOT NULL CHECK (basis IN ('assistant', 'power_of_attorney', 'guardian', 'executor', 'trustee', 'parent_of_minor')),
    instrument_reference TEXT,
    granted_by_person_id UUID NOT NULL REFERENCES person(id),
    granted_at TIMESTAMPTZ NOT NULL,
    subject_notified_at TIMESTAMPTZ,
    subject_notified_channel TEXT CHECK (subject_notified_channel IN ('in_person', 'telephone', 'post', 'email')),
    revoked_at TIMESTAMPTZ,
    revoked_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (delegate_person_id <> subject_person_id)
);

CREATE TABLE person_external_identity (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    system TEXT NOT NULL CHECK (system IN ('github', 'slack', 'docusign', 'google', 'linear', 'claude', 'chatgpt', 'notion')),
    external_id TEXT NOT NULL,
    handle TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (system, external_id),
    UNIQUE (person_id, system)
);

-- The record key is the claimed mailbox: a Person's lowercased email.
CREATE TABLE person_mailbox (
    id TEXT PRIMARY KEY CHECK (id = lower(id)),
    person_id UUID NOT NULL REFERENCES person(id)
);

CREATE INDEX address_person ON address (person_id);
CREATE INDEX address_entity ON address (entity_id);
CREATE INDEX email_token_person_purpose ON email_token (person_id, purpose);
CREATE INDEX entity_role_entity ON entity_role (entity_id);
CREATE INDEX person_delegate_pair ON person_delegate (delegate_person_id, subject_person_id);
CREATE INDEX person_delegate_subject ON person_delegate (subject_person_id);
CREATE INDEX person_mailbox_person ON person_mailbox (person_id);
