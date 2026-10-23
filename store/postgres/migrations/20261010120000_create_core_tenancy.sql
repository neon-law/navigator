-- First PostgreSQL target slice: reference rows and Firm → Brand → Project.
-- Each migration filename is a UTC YYYYMMDDHHMMSS timestamp plus a name.
-- SQLx's `_sqlx_migrations` is the only ledger of applied versions.

CREATE TABLE jurisdiction (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    code TEXT,
    jurisdiction_type TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (name)
);

CREATE TABLE entity_type (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE entity (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    entity_type_id UUID NOT NULL REFERENCES entity_type(id),
    jurisdiction_id UUID NOT NULL REFERENCES jurisdiction(id),
    phone TEXT,
    url TEXT,
    avatar_url TEXT,
    xero_id TEXT,
    firm_anchor_key TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE firm (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    entity_id UUID REFERENCES entity(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE brand (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    brand_key TEXT NOT NULL UNIQUE,
    firm_id UUID REFERENCES firm(id),
    primary_color TEXT,
    accent_color TEXT,
    typeface TEXT,
    is_law_firm BOOLEAN NOT NULL DEFAULT false,
    legal_entity TEXT,
    logo_object_key TEXT,
    logo_content_type TEXT,
    font_family TEXT,
    font_object_key TEXT,
    font_licence TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE project (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    brand TEXT,
    entity_id UUID NOT NULL REFERENCES entity(id),
    firm_id UUID REFERENCES firm(id),
    jurisdiction_id UUID REFERENCES jurisdiction(id),
    description TEXT,
    drive_folder_id TEXT,
    repository_url TEXT,
    git_initialized_at TEXT,
    forge_provisioned_at TEXT,
    closed_at TEXT,
    closure_reason TEXT,
    internal_slack_channel_url TEXT,
    external_slack_channel_url TEXT,
    internal_slack_channel_id TEXT,
    private_notion_page_url TEXT,
    shared_notion_page_url TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE person (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    given_name TEXT,
    family_name TEXT,
    middle_name TEXT,
    email TEXT NOT NULL,
    email_lower TEXT GENERATED ALWAYS AS (lower(email)) STORED,
    oidc_subject TEXT UNIQUE,
    microsoft_subject TEXT UNIQUE,
    apple_subject TEXT UNIQUE,
    role TEXT NOT NULL DEFAULT 'client',
    title TEXT,
    phone TEXT,
    xero_contact_id TEXT,
    profile_image_url TEXT,
    linkedin_url TEXT,
    email_confirmed BOOLEAN NOT NULL DEFAULT false,
    is_admitted BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (email_lower)
);

CREATE TABLE asset (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    storage_key TEXT NOT NULL,
    secondary_storage_key TEXT,
    content_type TEXT NOT NULL,
    byte_size BIGINT NOT NULL,
    sha256_hex TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    filename TEXT,
    kind TEXT,
    source TEXT,
    received_at TEXT,
    description TEXT,
    visibility TEXT NOT NULL DEFAULT 'internal',
    slug TEXT,
    published_at TEXT,
    source_message_id TEXT,
    source_sender TEXT,
    source_received_at TEXT,
    source_subject TEXT,
    metadata JSONB,
    derived_from JSONB,
    transcript_quality TEXT,
    signing JSONB,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE relationship_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_person_id UUID REFERENCES person(id),
    subject_type TEXT NOT NULL,
    subject_id UUID NOT NULL,
    action TEXT NOT NULL,
    detail TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
