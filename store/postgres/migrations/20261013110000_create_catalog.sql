-- The shared catalogs: Authority, the firm glossary, the Question catalog,
-- Notation packages with their ordered templates, and Notation rates. Each
-- is owned by no Firm or Project. Columns and unique natural keys keep the
-- Surreal source names, so the catalog mirror copies them one to one.
-- `notation_package_template.template_id` names a Template, which is not a
-- PostgreSQL table yet, so it carries no foreign key.
CREATE TABLE authority (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    class TEXT NOT NULL,
    citation TEXT NOT NULL UNIQUE,
    short_cite TEXT,
    title TEXT NOT NULL,
    publisher TEXT,
    issued_on TEXT,
    canonical_url TEXT,
    checked_on TEXT,
    archived_asset_id UUID REFERENCES asset(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE glossary_term (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE question (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE,
    prompt TEXT NOT NULL,
    answer_type TEXT NOT NULL,
    audience TEXT NOT NULL DEFAULT 'both' CHECK (audience IN ('lawyer', 'client', 'both')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notation_package (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT,
    complexity TEXT NOT NULL,
    starting_price_cents BIGINT NOT NULL,
    currency TEXT NOT NULL DEFAULT 'USD',
    active BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notation_package_template (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    package_id UUID NOT NULL REFERENCES notation_package(id),
    template_id UUID NOT NULL,
    position INTEGER NOT NULL,
    required BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (package_id, template_id),
    UNIQUE (package_id, position)
);

CREATE TABLE notation_rate (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    unit TEXT NOT NULL,
    unit_amount_cents BIGINT NOT NULL,
    currency TEXT NOT NULL DEFAULT 'USD',
    active BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
