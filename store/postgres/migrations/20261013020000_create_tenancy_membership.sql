-- Firm → Brand → Project membership: which Brands a Firm wears, which
-- practice membership a Person holds at a Firm, and which participation a
-- Person holds on a Project. Columns keep the Surreal source names so the
-- mirror and the parity manifest compare them one to one.
CREATE TABLE firm_brand (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT NOT NULL UNIQUE REFERENCES brand(brand_key),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (firm_id, brand_key)
);

CREATE TABLE person_firm_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    membership TEXT NOT NULL CHECK (membership IN ('admin', 'lawyer', 'clerk')),
    is_dri BOOLEAN NOT NULL DEFAULT false,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, firm_id)
);

CREATE TABLE person_project_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id),
    project_id UUID NOT NULL REFERENCES project(id),
    participation TEXT NOT NULL,
    is_lawyer_dri BOOLEAN NOT NULL DEFAULT false,
    is_client_dri BOOLEAN NOT NULL DEFAULT false,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, project_id)
);

CREATE INDEX person_firm_role_firm ON person_firm_role (firm_id);
CREATE INDEX person_project_role_project ON person_project_role (project_id);
