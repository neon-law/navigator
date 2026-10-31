-- Migration: 20261010120000_create_core_tenancy.sql
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

-- Migration: 20261010130000_create_post.sql
-- PostgreSQL's Brand-owned Post, named from the ontology and glossary.
-- The Surreal source table was also renamed from `blog_post` to `post`; this
-- target migration keeps the same public contract while PostgreSQL parity is
-- still being built table by table.
CREATE TABLE post (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    body_html TEXT NOT NULL DEFAULT '',
    body_markdown TEXT,
    status TEXT NOT NULL CHECK (status IN ('draft', 'scheduled', 'published', 'cancelled')),
    publish_at TIMESTAMPTZ,
    video_asset TEXT,
    tiktok_ai_generated BOOLEAN,
    vertical_video BOOLEAN NOT NULL DEFAULT false,
    video_orientation TEXT CHECK (video_orientation IS NULL OR video_orientation IN ('vertical', 'landscape', 'square')),
    source_digest TEXT,
    social_links JSONB NOT NULL DEFAULT '[]'::jsonb,
    facebook_caption TEXT,
    instagram_caption TEXT,
    threads_caption TEXT,
    youtube_caption TEXT,
    selected_social_accounts TEXT[] NOT NULL DEFAULT '{}',
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (brand_key, slug)
);

CREATE INDEX post_brand_status_publish_at ON post (brand_key, status, publish_at);

-- Migration: 20261011120000_create_social_handle_and_publication.sql
-- PostgreSQL's Social Handle and Social Publication, the two tables a Post
-- needs beside itself. A Social Handle is the public name a Brand holds on one
-- provider; a Post's social link may only name a handle recorded here. A Social
-- Publication is one Post's delivery state on one provider. The Surreal source
-- keys a publication by a stable string; here the row's identity is the unique
-- (post_id, provider) pair, which is what makes a repeated send idempotent.
CREATE TABLE social_handle (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    handle TEXT NOT NULL CHECK (handle ~ '^[A-Za-z0-9._-]{1,64}$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (brand_key, provider)
);

CREATE TABLE social_publication (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES post(id),
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    state TEXT NOT NULL CHECK (state IN ('sending', 'unknown', 'failed', 'confirmed')),
    attempt_count INTEGER NOT NULL CHECK (attempt_count > 0),
    provider_task_id TEXT,
    provider_post_id TEXT,
    permalink TEXT,
    last_error TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (post_id, provider)
);

-- Migration: 20261011200000_create_firm_journal.sql
-- Firm-owned journal. Existing Firm/Project authorization and Document storage
-- remain on their application seams while the tenancy migration is completed.
-- The SQLx runner owns the transaction, the migration lock and the ledger row.
CREATE TABLE IF NOT EXISTS journal_entry (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    command_key TEXT NOT NULL,
    actor_id UUID NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    memo TEXT NOT NULL,
    payload JSONB NOT NULL,
    digest TEXT NOT NULL,
    reversal_of UUID REFERENCES journal_entry(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    legacy_imported_at TIMESTAMPTZ,
    legacy_inserted_at_known BOOLEAN,
    legacy_inserted_at_source TEXT,
    UNIQUE (firm_id, command_key),
    UNIQUE (reversal_of),
    UNIQUE (id, firm_id)
);
CREATE TABLE IF NOT EXISTS journal_posting (
    entry_id UUID NOT NULL,
    firm_id UUID NOT NULL,
    position INTEGER NOT NULL,
    account TEXT NOT NULL,
    unit JSONB NOT NULL,
    quantity NUMERIC NOT NULL CHECK (quantity <> 0 AND scale(quantity) = 0 AND abs(quantity) <= 170141183460469231731687303715884105727),
    project_id UUID,
    category TEXT NOT NULL,
    ownership TEXT NOT NULL CHECK (ownership IN ('firm','client','unspecified')),
    restrictions JSONB NOT NULL CHECK (jsonb_typeof(restrictions) = 'array'),
    PRIMARY KEY (entry_id, position),
    FOREIGN KEY (entry_id, firm_id) REFERENCES journal_entry(id, firm_id)
);
CREATE TABLE IF NOT EXISTS journal_source_reference (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    provider TEXT NOT NULL,
    provider_scope TEXT NOT NULL,
    account TEXT NOT NULL,
    object_type TEXT NOT NULL,
    external_id TEXT NOT NULL,
    locator TEXT,
    payload JSONB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS journal_source_identity ON journal_source_reference
    (firm_id, provider, provider_scope, account, object_type, external_id, COALESCE(locator, ''));
CREATE TABLE IF NOT EXISTS journal_entry_source (
    entry_id UUID NOT NULL REFERENCES journal_entry(id),
    source_id TEXT NOT NULL REFERENCES journal_source_reference(id),
    PRIMARY KEY (entry_id, source_id)
);
CREATE TABLE IF NOT EXISTS journal_source_snapshot (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    source_id TEXT NOT NULL REFERENCES journal_source_reference(id),
    revision TEXT NOT NULL,
    digest TEXT NOT NULL,
    retrieved_at TIMESTAMPTZ NOT NULL,
    payload JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS journal_reconciliation (
    id UUID PRIMARY KEY,
    job_id UUID NOT NULL,
    firm_id UUID NOT NULL REFERENCES firm(id),
    mode TEXT NOT NULL CHECK (mode IN ('preview', 'approved_safe', 'reviewed_correction')),
    payload JSONB NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS journal_reconciliation_signal (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    source_id TEXT NOT NULL REFERENCES journal_source_reference(id),
    event_id TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (firm_id, source_id, event_id)
);
CREATE OR REPLACE FUNCTION journal_require_balance() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM journal_posting WHERE entry_id=NEW.id) < 2 OR EXISTS
       (SELECT 1 FROM journal_posting WHERE entry_id=NEW.id GROUP BY unit HAVING sum(quantity)<>0) THEN
        RAISE EXCEPTION 'journal entry must balance in each exact asset unit';
    END IF;
    RETURN NULL;
END;
$$;
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname='journal_entry_balanced') THEN
        CREATE CONSTRAINT TRIGGER journal_entry_balanced AFTER INSERT ON journal_entry
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION journal_require_balance();
    END IF;
END $$;
-- Ordinary runtime grants are SELECT/INSERT. The operator owns the migration.
-- These triggers preserve append-only records even if broader grants exist.
CREATE OR REPLACE FUNCTION journal_refuse_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'journal records are immutable'; END;
$$;
DO $$ DECLARE relation TEXT; BEGIN
    FOREACH relation IN ARRAY ARRAY['journal_entry','journal_posting','journal_source_reference','journal_entry_source','journal_source_snapshot','journal_reconciliation','journal_reconciliation_signal'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname=relation||'_immutable' AND tgrelid=relation::regclass) THEN
            EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION journal_refuse_mutation()',relation||'_immutable',relation);
        END IF;
    END LOOP;
END $$;

-- Migration: 20261012210000_create_clause.sql
-- The shared Clause library: legal prose a Template includes by `code@version`.
-- `rules/clauses/library.yaml` is the source of truth; this table is its read
-- model. Versioned by append: a change adds a row, and `is_current` marks the
-- live version of each code. A shared catalog, owned by no Firm or Project.
CREATE TABLE clause (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (code ~ '^[a-z0-9_]+$'),
    version INTEGER NOT NULL CHECK (version > 0),
    kind TEXT NOT NULL,
    inputs JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(inputs) = 'object'),
    body TEXT NOT NULL,
    body_sha256 TEXT NOT NULL CHECK (body_sha256 = encode(sha256(convert_to(body, 'UTF8')), 'hex')),
    is_current BOOLEAN NOT NULL DEFAULT false,
    retired_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (code, version),
    CHECK (NOT (is_current AND retired_at IS NOT NULL))
);

CREATE UNIQUE INDEX clause_one_current_per_code ON clause (code) WHERE is_current;

-- Migration: 20261013020000_create_tenancy_membership.sql
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

-- Migration: 20261013030000_add_brand_website_url.sql
-- A Brand's canonical website, `https://<host>` with a lowercase ASCII host.
-- Its host is the Brand's approved email domain; NULL approves none.
ALTER TABLE brand ADD COLUMN website_url TEXT CHECK (website_url ~ '^https://[a-z0-9-]+(\.[a-z0-9-]+)+$');

-- Migration: 20261013100000_create_identity_detail.sql
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

-- Migration: 20261013110000_create_catalog.sql
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

-- Migration: 20261013120001_add_asset_retention.sql
-- An asset's notarial acknowledgments, soft deletion, Legal Hold, and purge
-- tombstone, shaped as their SurrealDB source so the document mirror copies
-- them one to one. A soft deletion stamps every revision of a slug chain.
ALTER TABLE asset ADD COLUMN notarization JSONB;
ALTER TABLE asset ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN purge_after TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN deleted_by_person_id UUID REFERENCES person(id);
ALTER TABLE asset ADD COLUMN deleted_commit_sha TEXT;
ALTER TABLE asset ADD COLUMN legal_hold_at TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN purged_at TIMESTAMPTZ;

-- The dedup probe and the revision chain read, newest id first.
CREATE INDEX asset_sha256 ON asset (sha256_hex);
CREATE INDEX asset_project_slug ON asset (project_id, slug);

-- Migration: 20261013120002_create_notation_workflow.sql
-- A Notation, its append-only Notation Event journal, and the append-only
-- answers its questionnaire collects, shaped as their SurrealDB sources so
-- the workflow mirror copies them one to one. Every id is a UUIDv7 minted
-- by the writer, so `ORDER BY id` is insertion order: the newest event for a
-- `(notation_id, machine_kind)` pair is the current state, and the newest
-- answer for a question, state, and respondent wins.
--
-- `template_id` and `template_version_id` name Draft rows PostgreSQL does
-- not hold yet; they carry no foreign key until Template is ported.
CREATE TABLE notation (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL,
    package_id UUID REFERENCES notation_package(id),
    person_id UUID NOT NULL REFERENCES person(id),
    entity_id UUID REFERENCES entity(id),
    project_id UUID NOT NULL REFERENCES project(id),
    state TEXT NOT NULL,
    delivery TEXT NOT NULL DEFAULT 'embedded',
    questionnaire_snapshot JSONB,
    git_commit_sha TEXT,
    template_source_commit_sha TEXT,
    template_body_sha256 TEXT CHECK (template_body_sha256 ~ '^[0-9a-f]{64}$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notation_event (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notation_id UUID NOT NULL REFERENCES notation(id),
    acting_person_id UUID NOT NULL REFERENCES person(id),
    template_version_id UUID NOT NULL,
    machine_kind TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    condition TEXT NOT NULL,
    payload TEXT,
    recorded_at TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE answer (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    question_id UUID NOT NULL REFERENCES question(id),
    person_id UUID NOT NULL REFERENCES person(id),
    notation_id UUID REFERENCES notation(id),
    state_name TEXT,
    value JSONB NOT NULL,
    source TEXT NOT NULL DEFAULT 'lawyer' CHECK (source IN ('lawyer', 'client', 'extracted')),
    authored_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX notation_project ON notation (project_id);
CREATE INDEX notation_person ON notation (person_id);
CREATE INDEX notation_template ON notation (template_id);
CREATE INDEX notation_event_machine ON notation_event (notation_id, machine_kind, id);
CREATE INDEX answer_notation ON answer (notation_id, id);
CREATE INDEX answer_question_person ON answer (question_id, person_id);

-- Migration: 20261013130000_add_firm_slug.sql
-- A Firm's slug names its Forgejo organization (`<firm-slug>/<project-code>`).
-- Lowercase letters, digits, and single hyphens, at most 40 characters, unique
-- per installation, and immutable once written.
ALTER TABLE firm ADD COLUMN slug TEXT CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 40);

-- The derivation `store::firms::slug::derive` performs: NFKD, drop combining
-- marks and apostrophes, lowercase ASCII, one hyphen per run of anything else,
-- trimmed, cut to 40 characters without a trailing hyphen.
CREATE OR REPLACE FUNCTION firm_slug_from_name(name TEXT) RETURNS TEXT
    LANGUAGE sql IMMUTABLE STRICT
    AS $$
    SELECT rtrim(left(trim(BOTH '-' FROM regexp_replace(lower(regexp_replace(
        normalize(name, NFKD),
        '[̀-ͯ᪰-᫿᷀-᷿⃐-⃿︠-︯''’ʼ]', '', 'g')),
        '[^a-z0-9]+', '-', 'g')), 40), '-')
$$;

CREATE OR REPLACE FUNCTION firm_slug_immutable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.slug IS DISTINCT FROM OLD.slug THEN
        RAISE EXCEPTION 'firm.slug is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- Backfill existing Firms. An invalid derived slug fails the CHECK and two
-- Firms deriving one slug fail the unique index: the migration stops rather
-- than suffixing.
DO $$
BEGIN
    UPDATE firm SET slug = firm_slug_from_name(name) WHERE slug IS NULL;
    ALTER TABLE firm ALTER COLUMN slug SET NOT NULL;
    CREATE TRIGGER firm_slug_immutable BEFORE UPDATE OF slug ON firm
        FOR EACH ROW EXECUTE FUNCTION firm_slug_immutable();
END $$;

CREATE UNIQUE INDEX firm_slug ON firm (slug);

-- Migration: 20261015203853_create_summary_sender_approvals.sql
-- Firm-scoped authorization state and its append-only audit trail.
CREATE UNIQUE INDEX project_id_firm_id ON project (id, firm_id);

CREATE TABLE summary_sender_approval (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    project_id UUID,
    provider_account TEXT NOT NULL,
    person_id UUID NOT NULL REFERENCES person(id),
    sender_email TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('approved', 'revoked')),
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id)
);

CREATE UNIQUE INDEX summary_sender_approval_identity
    ON summary_sender_approval (firm_id, project_id, provider_account, sender_email);
CREATE INDEX summary_sender_approval_firm
    ON summary_sender_approval (firm_id, updated_at DESC);

CREATE TABLE summary_sender_approval_event (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    project_id UUID,
    provider_account TEXT NOT NULL,
    sender_email TEXT NOT NULL,
    person_id UUID NOT NULL REFERENCES person(id),
    action TEXT NOT NULL CHECK (action IN ('approved', 'revoked')),
    inserted_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id)
);
CREATE INDEX summary_sender_approval_event_firm
    ON summary_sender_approval_event (firm_id, inserted_at DESC);

-- Migration: 20261015220000_create_project_deadlines.sql
-- Project-scoped Deadline records. The Project FK is the ownership boundary;
-- linked Documents and owners remain optional as in the Surreal source.
CREATE TABLE project_deadlines (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    due_at TIMESTAMPTZ NOT NULL,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('statute', 'court_rule', 'court_order', 'contract', 'internal')),
    authority TEXT NOT NULL,
    source TEXT NOT NULL,
    source_document_id UUID REFERENCES asset(id),
    owner_person_id UUID REFERENCES person(id),
    status TEXT NOT NULL CHECK (status IN ('open', 'complete', 'cancelled')),
    calendar_role TEXT NOT NULL CHECK (calendar_role IN ('firm', 'opponent', 'hearing')),
    internal_lead_days BIGINT CHECK (internal_lead_days >= 0),
    client_lead_days BIGINT CHECK (client_lead_days >= 0),
    replay_key TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK (client_lead_days IS NULL OR internal_lead_days IS NOT NULL),
    CHECK (client_lead_days IS NULL OR internal_lead_days >= client_lead_days)
);
CREATE UNIQUE INDEX project_deadlines_replay_key
    ON project_deadlines (project_id, replay_key);
CREATE INDEX project_deadlines_due_at
    ON project_deadlines (project_id, status, due_at);

-- Migration: 20261016100000_create_project_costs.sql
-- Project-scoped third-party costs; Xero remains the invoice system of record.
CREATE UNIQUE INDEX asset_project_id_id ON asset(project_id,id);
CREATE TABLE project_costs (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    occurred_on DATE NOT NULL,
    payee TEXT NOT NULL CHECK (length(btrim(payee)) BETWEEN 1 AND 300),
    description TEXT NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 2000),
    amount_cents BIGINT NOT NULL CHECK (amount_cents > 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    amount_status TEXT NOT NULL CHECK (amount_status IN ('estimated', 'confirmed')),
    billing_status TEXT NOT NULL CHECK (billing_status IN ('unbilled', 'invoiced', 'resolved')),
    receipt_document_id UUID NOT NULL,
    xero_invoice_id TEXT,
    idempotency_payload JSONB NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(btrim(idempotency_key)) BETWEEN 1 AND 200),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, idempotency_key),
    UNIQUE (project_id, receipt_document_id),
    FOREIGN KEY (project_id,receipt_document_id) REFERENCES asset(project_id,id),
    CHECK ((billing_status = 'invoiced') = (xero_invoice_id IS NOT NULL))
);
CREATE INDEX project_costs_project_date ON project_costs(project_id, occurred_on, id);

CREATE TABLE project_cost_events (
    id UUID PRIMARY KEY,
    project_cost_id UUID NOT NULL REFERENCES project_costs(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'amount_confirmed', 'invoice_linked', 'resolved')),
    before_state JSONB,
    after_state JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_cost_events_cost_time ON project_cost_events(project_cost_id, occurred_at, id);

CREATE FUNCTION enforce_project_cost_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Project costs are retained; resolve a cost instead of deleting it';
    END IF;
    actor_id := nullif(current_setting('navigator.project_cost_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.project_cost_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('amount_confirmed', 'invoice_linked', 'resolved') THEN
        RAISE EXCEPTION 'Project cost updates require an actor and audit event';
    END IF;
    INSERT INTO project_cost_events(id,project_cost_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),
        to_jsonb(NEW) || CASE WHEN nullif(current_setting('navigator.project_cost_event_detail', true), '') IS NOT NULL
            THEN jsonb_build_object('reason',current_setting('navigator.project_cost_event_detail', true))
            ELSE '{}'::jsonb END,now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_costs_audit_update
    BEFORE UPDATE OR DELETE ON project_costs
    FOR EACH ROW EXECUTE FUNCTION enforce_project_cost_audit();

CREATE FUNCTION record_project_cost_creation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
BEGIN
    actor_id := nullif(current_setting('navigator.project_cost_actor', true), '')::UUID;
    IF actor_id IS NULL OR current_setting('navigator.project_cost_event_kind', true) IS DISTINCT FROM 'created' THEN
        RAISE EXCEPTION 'Project cost creation requires an actor and audit event';
    END IF;
    INSERT INTO project_cost_events(id,project_cost_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,'created',NULL,
        to_jsonb(NEW) || jsonb_build_object('request',NEW.idempotency_payload),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_costs_audit_create
    AFTER INSERT ON project_costs
    FOR EACH ROW EXECUTE FUNCTION record_project_cost_creation();

CREATE FUNCTION reject_project_cost_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Project cost audit events are immutable';
END;
$$;
CREATE TRIGGER project_cost_events_immutable
    BEFORE UPDATE OR DELETE ON project_cost_events
    FOR EACH ROW EXECUTE FUNCTION reject_project_cost_event_mutation();

-- Migration: 20261016110000_create_communications_conversations.sql
-- SurrealDB remains the write authority; these tables are an opt-in mirror.
CREATE TABLE communication (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    channel TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound', 'internal')),
    author_person_id UUID REFERENCES person(id),
    counterparty TEXT,
    subject TEXT,
    body TEXT NOT NULL,
    source_ref TEXT,
    asset_id UUID REFERENCES asset(id),
    occurred_at TIMESTAMPTZ NOT NULL,
    call JSONB,
    agreements JSONB NOT NULL DEFAULT '[]'::jsonb,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX communication_channel_source_ref
    ON communication (channel, source_ref) WHERE source_ref IS NOT NULL;
CREATE INDEX communication_project_occurred ON communication (project_id, occurred_at);

CREATE TABLE email_conversation (
    id UUID PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    external_email TEXT NOT NULL,
    external_name TEXT,
    person_id UUID REFERENCES person(id),
    subject TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'awaiting_lawyer', 'awaiting_client', 'closed')),
    notation_id UUID REFERENCES notation(id),
    firm_id UUID REFERENCES firm(id),
    project_id UUID REFERENCES project(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_conversation_external_email ON email_conversation (external_email);
CREATE INDEX email_conversation_scope ON email_conversation (firm_id, project_id);

CREATE TABLE email_conversation_message (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES email_conversation(id) ON DELETE CASCADE,
    direction TEXT NOT NULL CHECK (direction IN ('from_external', 'to_lawyer', 'from_lawyer', 'to_external', 'system')),
    from_addr TEXT NOT NULL,
    to_addr TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    raw_storage_key TEXT,
    provider_message_id TEXT,
    in_reply_to TEXT,
    command_payload TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_conversation_message_conversation ON email_conversation_message (conversation_id);

-- Migration: 20261016120000_create_template.sql
-- Versioned Template metadata. Bodies remain in their content-addressed Asset.
CREATE TABLE template (
    id UUID PRIMARY KEY,
    code TEXT NOT NULL,
    title TEXT NOT NULL,
    respondent_type TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    is_current BOOLEAN NOT NULL,
    asset_id UUID REFERENCES asset(id),
    form_code TEXT,
    kind TEXT,
    source_commit_sha TEXT,
    body_sha256 TEXT CHECK (body_sha256 IS NULL OR body_sha256 ~ '^[0-9a-f]{64}$'),
    current_key TEXT UNIQUE,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX template_code ON template (code);

-- Migration: 20261016123000_create_sent_email.sql
-- Outbound audit is append-only. PostgreSQL mirrors never overwrite history.
CREATE TABLE sent_email (
    id UUID PRIMARY KEY,
    recipient TEXT NOT NULL,
    subject TEXT NOT NULL,
    sender TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    template_slug TEXT,
    body TEXT NOT NULL,
    outcome TEXT NOT NULL,
    sg_message_id TEXT,
    sent_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX sent_email_project_sent_at ON sent_email (project_id, sent_at DESC, id DESC);

-- Migration: 20261016130000_add_workflow_template_pins.sql
-- Every workflow and package pins an existing, retained Template version.
-- An orphan stops this transactional migration for source reconciliation.
ALTER TABLE notation ADD CONSTRAINT notation_template_version FOREIGN KEY (template_id) REFERENCES template(id);
ALTER TABLE notation_event ADD CONSTRAINT notation_event_template_version FOREIGN KEY (template_version_id) REFERENCES template(id);
ALTER TABLE notation_package_template ADD CONSTRAINT notation_package_template_version FOREIGN KEY (template_id) REFERENCES template(id);

-- Migration: 20261016140000_create_project_service.sql
-- Project-scoped proof that a filed Document reached a recipient.
CREATE TABLE project_service (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    document_id UUID NOT NULL REFERENCES asset(id),
    recipient TEXT NOT NULL,
    recipient_person_id UUID REFERENCES person(id),
    recipient_entity_id UUID REFERENCES entity(id),
    counsel_for TEXT,
    method TEXT NOT NULL CHECK (method IN ('personal', 'mail', 'certified_mail', 'overnight', 'e_service', 'electronic_by_agreement')),
    served_on DATE NOT NULL,
    address TEXT NOT NULL,
    declarant TEXT NOT NULL,
    declarant_person_id UUID REFERENCES person(id),
    letter_id UUID,
    sent_email_id UUID REFERENCES sent_email(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK (recipient_person_id IS NULL OR recipient_entity_id IS NULL)
);
CREATE INDEX project_service_project_document ON project_service (project_id, document_id);

-- Migration: 20261017020000_create_project_reference.sql
-- A verified Project party retained for conflict checks. This row is not
-- participation and conveys no access to the Project.
CREATE TABLE project_reference (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id),
    person_id UUID REFERENCES person(id),
    entity_id UUID REFERENCES entity(id),
    reason TEXT NOT NULL,
    CONSTRAINT project_reference_reason CHECK (reason IN ('counterparty_officer', 'registered_agent', 'signatory', 'adverse_party', 'counterparty_entity')),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK ((person_id IS NOT NULL) <> (entity_id IS NOT NULL))
);

CREATE UNIQUE INDEX project_reference_person_tie
    ON project_reference (project_id, person_id, reason)
    WHERE person_id IS NOT NULL;

CREATE UNIQUE INDEX project_reference_entity_tie
    ON project_reference (project_id, entity_id, reason)
    WHERE entity_id IS NOT NULL;

-- Migration: 20261017030000_create_email_receipts_and_delivery.sql
-- SurrealDB remains the write authority; these tables are an opt-in mirror.
CREATE TABLE email_receipt (
    id UUID PRIMARY KEY,
    receiving_mailbox TEXT NOT NULL,
    deployment TEXT NOT NULL,
    firm_id UUID REFERENCES firm(id),
    provider_account TEXT,
    sender_authenticated BOOLEAN NOT NULL DEFAULT false,
    raw_digest TEXT NOT NULL,
    source_message_id TEXT,
    archive_key TEXT NOT NULL,
    letter_id UUID NOT NULL,
    processing_state TEXT NOT NULL CHECK (processing_state IN ('pending', 'archived')),
    delivery_state TEXT NOT NULL CHECK (delivery_state = 'not_attempted'),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX email_receipt_identity
    ON email_receipt (receiving_mailbox, deployment, raw_digest);
CREATE INDEX email_receipt_firm ON email_receipt (firm_id, inserted_at DESC);

CREATE TABLE email_delivery (
    id UUID PRIMARY KEY,
    receipt_id UUID NOT NULL UNIQUE REFERENCES email_receipt(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('not_attempted', 'sending', 'unknown', 'confirmed', 'failed')),
    current_correlation_id TEXT,
    slack_timestamp TEXT,
    last_error TEXT,
    reconciled_by TEXT,
    reconciliation TEXT,
    reconciled_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE email_delivery_attempt (
    id UUID PRIMARY KEY,
    delivery_id UUID NOT NULL REFERENCES email_delivery(id) ON DELETE CASCADE,
    correlation_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('sending', 'unknown', 'confirmed', 'failed')),
    slack_timestamp TEXT,
    error TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_delivery_attempt_delivery ON email_delivery_attempt (delivery_id, inserted_at);

-- Migration: 20261017100000_create_social_account.sql
-- A Brand's encrypted provider connection. The source key is the stable
-- Surreal record key; ciphertext and wrapped key stay byte-for-byte intact.
CREATE UNIQUE INDEX brand_key_firm_id ON brand (brand_key, firm_id);

CREATE TABLE social_account (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    provider_account_id TEXT NOT NULL,
    display_name TEXT,
    scopes TEXT[] NOT NULL,
    expires_at TIMESTAMPTZ,
    ciphertext TEXT NOT NULL,
    wrapped_dek TEXT NOT NULL,
    kms_key_version TEXT NOT NULL,
    kms_context TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('connected', 'revoked')),
    connected_by_person_id UUID NOT NULL REFERENCES person(id),
    revoked_by_person_id UUID REFERENCES person(id),
    revoked_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (brand_key, firm_id) REFERENCES brand(brand_key, firm_id),
    UNIQUE (brand_key, provider, provider_account_id),
    CHECK ((status = 'connected' AND revoked_at IS NULL AND revoked_by_person_id IS NULL) OR (status = 'revoked' AND revoked_at IS NOT NULL AND revoked_by_person_id IS NOT NULL)),
    CHECK ((status = 'connected' AND ciphertext <> '' AND wrapped_dek <> '') OR (status = 'revoked' AND ciphertext = '' AND wrapped_dek = ''))
);

CREATE INDEX social_account_firm_status ON social_account (firm_id, status, provider);

-- Migration: 20261017110000_create_mailrooms_letters.sql
-- Physical mail destinations and pieces of mail. SurrealDB remains the
-- source of truth while these tables receive bounded best-effort mirrors.
CREATE TABLE mailroom (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    address_id UUID NOT NULL REFERENCES address(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE letter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    mailroom_id UUID NOT NULL REFERENCES mailroom(id),
    project_id UUID,
    direction TEXT NOT NULL CHECK (direction IN ('incoming', 'outgoing')),
    sender TEXT NOT NULL,
    recipient TEXT NOT NULL,
    summary TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX letter_project_id ON letter (project_id);

-- Migration: 20261017120000_create_leads.sql
-- SurrealDB remains the write authority; these tables are opt-in mirrors.
CREATE TABLE lead (
    id UUID PRIMARY KEY,
    email TEXT NOT NULL,
    email_lower TEXT NOT NULL,
    phone TEXT,
    brand_key TEXT NOT NULL,
    source_path TEXT NOT NULL,
    consent_version TEXT NOT NULL,
    consented_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('new', 'contacted', 'converted', 'declined', 'unsubscribed')),
    unsubscribed_at TIMESTAMPTZ,
    person_id UUID,
    submissions BIGINT NOT NULL CHECK (submissions >= 1),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX lead_email_lower ON lead (email_lower);
CREATE INDEX lead_brand_key ON lead (brand_key);

CREATE TABLE lead_consent (
    id UUID PRIMARY KEY,
    lead_id UUID NOT NULL,
    consented_at TIMESTAMPTZ NOT NULL,
    phone TEXT,
    source_path TEXT NOT NULL,
    sms_consent_version TEXT NOT NULL,
    sms_policy_version TEXT NOT NULL,
    sms_opt_in BOOLEAN NOT NULL
);
CREATE INDEX lead_consent_lead ON lead_consent (lead_id);

CREATE FUNCTION reject_lead_consent_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Lead consent evidence is immutable';
END;
$$;
CREATE TRIGGER lead_consent_immutable
    BEFORE UPDATE OR DELETE ON lead_consent
    FOR EACH ROW EXECUTE FUNCTION reject_lead_consent_mutation();

-- Migration: 20261018120000_create_matter_pages_and_captions.sql
-- A Project's synced `pages:` entries and `matter:` block. SurrealDB remains
-- the write authority; these tables are opt-in mirrors. The compound
-- Project/Firm key proves each row belongs to its Project's Firm and follows
-- the Project when it moves.
CREATE TABLE matter_page (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    slug TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    draft TEXT,
    position BIGINT NOT NULL CHECK (position >= 0),
    client_sections TEXT[],
    lawyer_sections TEXT[],
    clerk_sections TEXT[],
    synced_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, slug) DEFERRABLE INITIALLY DEFERRED,
    UNIQUE (project_id, position) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE
);

CREATE TABLE matter_caption (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL UNIQUE REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    court TEXT[] NOT NULL,
    case_number TEXT NOT NULL,
    judicial_officer TEXT NOT NULL,
    department TEXT NOT NULL,
    filed_on TEXT NOT NULL,
    trial_on TEXT,
    parties JSONB NOT NULL CHECK (jsonb_typeof(parties) = 'array'),
    counsel JSONB NOT NULL CHECK (jsonb_typeof(counsel) = 'object'),
    synced_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE
);

-- Migration: 20261018150000_create_project_archive.sql
-- Project Recovery Archive generations in the private archived-projects bucket.
-- Born in PostgreSQL. A generation is committed only after every expected item
-- was copied, read back, and verified by size and SHA-256, and its
-- content-addressed manifest was written and verified the same way.
CREATE TABLE project_archive_generation (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_key TEXT NOT NULL UNIQUE CHECK (operation_key <> ''),
    state TEXT NOT NULL CHECK (state IN ('pending', 'copying', 'committed')),
    requested_by_person_id UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    source_backup JSONB,
    cutoff_at TIMESTAMPTZ,
    inventory_sha256 TEXT CHECK (inventory_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_key TEXT,
    manifest_sha256 TEXT CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_size_bytes BIGINT CHECK (manifest_size_bytes > 0),
    item_count INTEGER CHECK (item_count >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    committed_at TIMESTAMPTZ,
    CHECK ((state = 'committed') = (committed_at IS NOT NULL AND manifest_key IS NOT NULL AND manifest_sha256 IS NOT NULL AND manifest_size_bytes IS NOT NULL AND item_count IS NOT NULL AND inventory_sha256 IS NOT NULL AND cutoff_at IS NOT NULL))
);
CREATE INDEX project_archive_generation_project ON project_archive_generation (project_id, state);
CREATE INDEX project_archive_generation_firm ON project_archive_generation (firm_id);

-- One expected item of a generation and its copy checkpoint.
CREATE TABLE project_archive_item (
    generation_id UUID NOT NULL REFERENCES project_archive_generation(id),
    path TEXT NOT NULL CHECK (path <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('document_revision', 'notation_source', 'notation_output', 'restore_metadata', 'source_backup')),
    asset_id UUID,
    source_key TEXT,
    object_key TEXT NOT NULL CHECK (object_key <> ''),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    verified_at TIMESTAMPTZ,
    PRIMARY KEY (generation_id, path)
);
CREATE INDEX project_archive_item_object ON project_archive_item (object_key);

CREATE FUNCTION reject_committed_archive_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'committed' THEN
        RAISE EXCEPTION 'A committed Project Recovery Archive generation is immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_archive_generation_immutable
    BEFORE UPDATE OR DELETE ON project_archive_generation
    FOR EACH ROW EXECUTE FUNCTION reject_committed_archive_mutation();

CREATE FUNCTION reject_committed_archive_item_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM project_archive_generation WHERE id = OLD.generation_id AND state = 'committed') THEN
        RAISE EXCEPTION 'Items of a committed Project Recovery Archive generation are immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_archive_item_immutable
    BEFORE UPDATE OR DELETE ON project_archive_item
    FOR EACH ROW EXECUTE FUNCTION reject_committed_archive_item_mutation();

-- Migration: 20261018160000_create_project_source_import.sql
-- A durable request to initialize a Project's source repository from a
-- validated headless source archive. Born in PostgreSQL. The canonical ZIP is
-- the recovery source a retry after worker loss or an unknown forge outcome
-- re-reads; it is cleared once the commit is verified and recorded.
CREATE TABLE project_source_import (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_key TEXT NOT NULL UNIQUE CHECK (operation_key <> ''),
    tree_sha256 TEXT NOT NULL CHECK (tree_sha256 ~ '^[0-9a-f]{64}$'),
    file_count INTEGER NOT NULL CHECK (file_count > 0),
    canonical_zip BYTEA,
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'refused')),
    requested_by_person_id UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    repository_url TEXT,
    commit_sha TEXT CHECK (commit_sha ~ '^[0-9a-f]{40,64}$'),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    finished_at TIMESTAMPTZ,
    CHECK ((state = 'pending') = (canonical_zip IS NOT NULL)),
    CHECK ((state = 'committed') = (commit_sha IS NOT NULL AND repository_url IS NOT NULL AND finished_at IS NOT NULL))
);
CREATE UNIQUE INDEX project_source_import_one_pending ON project_source_import (project_id) WHERE state = 'pending';
CREATE INDEX project_source_import_state ON project_source_import (state, requested_at);

-- Migration: 20261018170000_create_document_retention.sql
-- Soft-deleted document chains in the private archived-projects lane, and the
-- governed retention purge's own accounting. Born in PostgreSQL.

-- One archive of one soft-deleted chain, per soft deletion.
CREATE TABLE document_chain_archive (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    chain_key TEXT NOT NULL CHECK (chain_key <> ''),
    deleted_at TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('archiving', 'archived', 'restored', 'purged')),
    requested_at TIMESTAMPTZ NOT NULL,
    archived_at TIMESTAMPTZ,
    active_removed_at TIMESTAMPTZ,
    restored_at TIMESTAMPTZ,
    purged_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    UNIQUE (project_id, chain_key, deleted_at),
    CHECK (state = 'archiving' OR archived_at IS NOT NULL),
    CHECK (active_removed_at IS NULL OR archived_at IS NOT NULL)
);
CREATE INDEX document_chain_archive_state ON document_chain_archive (state);

-- One revision's verified copy.
CREATE TABLE document_chain_archive_item (
    archive_id UUID NOT NULL REFERENCES document_chain_archive(id),
    asset_id UUID NOT NULL,
    storage_key TEXT NOT NULL,
    object_key TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    verified_at TIMESTAMPTZ,
    PRIMARY KEY (archive_id, asset_id)
);
CREATE INDEX document_chain_archive_item_asset ON document_chain_archive_item (asset_id);

-- One governed purge of one chain past its window, keyed so a retry after a
-- crash joins the same run rather than starting another.
CREATE TABLE retention_purge (
    project_id UUID NOT NULL,
    chain_key TEXT NOT NULL CHECK (chain_key <> ''),
    purge_after TIMESTAMPTZ NOT NULL,
    firm_id UUID NOT NULL,
    authorized_by_person_id UUID NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    expunge_record_id UUID,
    finished_at TIMESTAMPTZ,
    PRIMARY KEY (project_id, chain_key, purge_after)
);

-- Archived bytes the retention purge removed, so recovery generations that
-- held them account for the removal instead of reporting corruption.
CREATE TABLE archived_object_purge (
    project_id UUID NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    purged_at TIMESTAMPTZ NOT NULL,
    expunge_record_id UUID,
    PRIMARY KEY (project_id, sha256)
);

-- Migration: 20261018180000_create_project_handoff.sql
-- A legal-files-only handoff to one recipient: the immutable manifest of the
-- exact revisions, sizes, and checksums the recipient may receive. Born in
-- PostgreSQL; the bytes stay in Navigator storage.
CREATE TABLE project_handoff (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    recipient_person_id UUID NOT NULL,
    lens TEXT NOT NULL CHECK (lens IN ('client', 'lawyer')),
    required BOOLEAN NOT NULL,
    manifest JSONB NOT NULL,
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    total_bytes BIGINT NOT NULL CHECK (total_bytes >= 0),
    created_by_person_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_handoff_project ON project_handoff (project_id, created_at);

CREATE FUNCTION reject_handoff_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'A handoff manifest is immutable';
END;
$$;
CREATE TRIGGER project_handoff_immutable
    BEFORE UPDATE OR DELETE ON project_handoff
    FOR EACH ROW EXECUTE FUNCTION reject_handoff_mutation();

-- Migration: 20261018190000_create_project_handoff_acknowledgment.sql
-- The recipient's acknowledgment of one handoff, bound to the exact manifest
-- digest they received. Born in PostgreSQL; immutable once written.
CREATE TABLE project_handoff_acknowledgment (
    handoff_id UUID PRIMARY KEY REFERENCES project_handoff(id),
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    acknowledged_by_person_id UUID NOT NULL,
    acknowledged_at TIMESTAMPTZ NOT NULL
);

CREATE TRIGGER project_handoff_acknowledgment_immutable
    BEFORE UPDATE OR DELETE ON project_handoff_acknowledgment
    FOR EACH ROW EXECUTE FUNCTION reject_handoff_mutation();

-- Migration: 20261020010000_create_inbox.sql
-- SurrealDB remains the write authority; Inbox rows are opt-in mirrors.
CREATE TABLE inbox_conversation (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT,
    interaction_type TEXT NOT NULL DEFAULT 'message',
    provider_post_id TEXT,
    project_id UUID REFERENCES project(id),
    channel TEXT NOT NULL CHECK (channel IN ('email', 'sms', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok')),
    external_address TEXT NOT NULL,
    external_name TEXT,
    subject TEXT,
    unread BOOLEAN NOT NULL DEFAULT TRUE,
    provider_account TEXT NOT NULL,
    provider_thread_ref TEXT,
    source_page TEXT,
    source_channel TEXT,
    matter_consent BOOLEAN NOT NULL DEFAULT FALSE,
    promotional_opt_in BOOLEAN NOT NULL DEFAULT FALSE,
    promotional_opt_out BOOLEAN NOT NULL DEFAULT FALSE,
    email_conversation_id UUID REFERENCES email_conversation(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_conversation_firm ON inbox_conversation (firm_id);
CREATE INDEX inbox_conversation_email ON inbox_conversation (email_conversation_id);

CREATE TABLE message (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES inbox_conversation(id),
    parent_message_id UUID REFERENCES message(id),
    visibility TEXT CHECK (visibility IN ('public', 'private')),
    direction TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound', 'internal')),
    body TEXT NOT NULL,
    author_person_id UUID REFERENCES person(id),
    provider_message_id TEXT,
    provider_ref TEXT,
    delivery_state TEXT NOT NULL CHECK (delivery_state IN ('received', 'queued', 'submitted', 'delivered', 'unknown', 'failed', 'suppressed')),
    purpose TEXT NOT NULL DEFAULT 'matter' CHECK (purpose IN ('matter', 'promotional')),
    attachment_asset_id UUID REFERENCES asset(id),
    communication_id UUID REFERENCES communication(id),
    occurred_at TEXT NOT NULL,
    alert_state TEXT CHECK (alert_state IN ('pending', 'notified', 'unknown', 'failed', 'disabled')),
    alert_channel_id TEXT,
    alert_timestamp TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX message_conversation ON message (conversation_id);

CREATE TABLE inbox_ingress (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    channel TEXT NOT NULL,
    provider_account TEXT NOT NULL,
    provider_message_id TEXT NOT NULL,
    message_id UUID NOT NULL REFERENCES message(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT inbox_ingress_key UNIQUE (firm_id, channel, provider_account, provider_message_id)
);

CREATE TABLE inbox_assignment (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES inbox_conversation(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    from_project_id UUID REFERENCES project(id),
    to_project_id UUID NOT NULL REFERENCES project(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_assignment_conversation ON inbox_assignment (conversation_id);

CREATE TABLE inbox_delivery_attempt (
    id UUID PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES message(id),
    state TEXT NOT NULL,
    provider_ref TEXT,
    detail TEXT,
    inserted_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_delivery_attempt_message ON inbox_delivery_attempt (message_id);

CREATE FUNCTION reject_inbox_audit_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Inbox audit rows are immutable';
END;
$$;
CREATE TRIGGER inbox_ingress_immutable
    BEFORE UPDATE OR DELETE ON inbox_ingress
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
CREATE TRIGGER inbox_assignment_immutable
    BEFORE UPDATE OR DELETE ON inbox_assignment
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
CREATE TRIGGER inbox_delivery_attempt_immutable
    BEFORE UPDATE OR DELETE ON inbox_delivery_attempt
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();

-- Migration: 20261020100000_create_firm_integration_secret.sql
CREATE TABLE firm_integration_secret (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    ciphertext TEXT NOT NULL,
    wrapped_dek TEXT NOT NULL,
    kms_key_version TEXT NOT NULL,
    kms_context TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'revoked')),
    actor_id UUID NOT NULL REFERENCES person(id),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (firm_id, provider, kind, version)
);

CREATE INDEX firm_integration_secret_active
    ON firm_integration_secret (firm_id, provider, kind, version DESC)
    WHERE status = 'active';

-- Migration: 20261020120000_create_purchase.sql
-- A Client's Purchase of an Offering. Born in PostgreSQL with no Surreal
-- source. The Offering, its price, and its cadence are copied from the
-- services catalog at the moment of purchase, so a later price change never
-- rewrites what a Client was invoiced. Xero raises and collects the invoice;
-- this row records which invoice answers for the Purchase and turns `active`
-- once Xero reports it paid.
CREATE TABLE purchase (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL,
    email TEXT NOT NULL CHECK (email <> ''),
    offering TEXT NOT NULL CHECK (offering ~ '^[a-z0-9-]{1,64}$'),
    offering_name TEXT NOT NULL CHECK (offering_name <> ''),
    cadence TEXT NOT NULL CHECK (cadence IN ('once', 'month', 'year')),
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    account_code TEXT NOT NULL CHECK (account_code ~ '^[A-Za-z0-9]{1,10}$'),
    state TEXT NOT NULL CHECK (state IN ('complimentary', 'invoicing', 'invoiced', 'active')),
    xero_contact_id TEXT,
    xero_invoice_id TEXT UNIQUE,
    invoice_url TEXT,
    xero_repeating_invoice_id TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    activated_at TIMESTAMPTZ,
    CHECK ((state = 'complimentary') = (amount_cents = 0)),
    CHECK (state NOT IN ('invoiced', 'active') OR xero_invoice_id IS NOT NULL),
    CHECK ((state = 'active') = (activated_at IS NOT NULL))
);
-- At most one unpaid invoice per Client and Offering, so a double click or a
-- retry finds the open Purchase instead of raising a second invoice.
CREATE UNIQUE INDEX purchase_one_open ON purchase (person_id, offering) WHERE state IN ('invoicing', 'invoiced');
CREATE INDEX purchase_person ON purchase (person_id, inserted_at);
CREATE INDEX purchase_invoiced ON purchase (state, updated_at) WHERE state = 'invoiced';

-- Migration: 20261020130000_create_notation_clause.sql
-- Per-Notation custom prose, retained in render order with its author.
CREATE TABLE notation_clause (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    body_markdown TEXT NOT NULL,
    authored_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX notation_clause_notation ON notation_clause (notation_id, position, id);

-- Migration: 20261021010000_create_attestation.sql
CREATE TABLE attestation (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE RESTRICT,
    chain TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    pda TEXT,
    tx_signature TEXT,
    firm_wallet TEXT,
    client_wallet TEXT,
    recorded_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

-- Migration: 20261021130000_create_social_account_selection.sql
-- A saved destination has no credentials or publishing authority.
CREATE TABLE social_account_selection (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    provider_account_id TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (brand_key, firm_id) REFERENCES brand(brand_key, firm_id),
    UNIQUE (brand_key, provider)
);

-- Migration: 20261021140000_create_project_records.sql
-- Project-scoped records and a client Entity's playbooks, mirrored from
-- SurrealDB under their record UUIDs.
CREATE TABLE project_module (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    module TEXT NOT NULL CHECK (module IN ('litigation', 'cap_table', 'estate', 'deadlines')),
    enabled_at TIMESTAMPTZ NOT NULL,
    enabled_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, module)
);

CREATE TABLE statutory_deadline (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    trigger_on DATE NOT NULL,
    due_on DATE NOT NULL,
    statute TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, kind, trigger_on, source)
);

CREATE TABLE testimonial (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    person_id UUID NOT NULL REFERENCES person(id),
    quote TEXT NOT NULL,
    attribution_label TEXT,
    consented_at TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    display_order INTEGER NOT NULL DEFAULT 0,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, person_id, quote)
);

CREATE TABLE playbook (
    id UUID PRIMARY KEY,
    entity_id UUID NOT NULL REFERENCES entity(id),
    name TEXT NOT NULL,
    positions JSONB,
    active BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (entity_id, name)
);

-- Migration: 20261021143000_create_notation_inputs.sql
-- A Notation's ordered intake sources and a Project's unrun Notation drafts,
-- mirrored from SurrealDB under their record UUIDs. A source names its
-- Communication, never a copy of the message.
CREATE TABLE notation_source (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    communication_id UUID REFERENCES communication(id),
    source_ref TEXT,
    position INTEGER NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (notation_id, position)
);

CREATE INDEX notation_source_communication ON notation_source (communication_id);

CREATE TABLE notation_draft (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    source TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX notation_draft_project ON notation_draft (project_id);

-- Migration: 20261021150000_create_git_access.sql
-- Tracked Git repositories and Git access tokens, mirrored from SurrealDB
-- under their record UUIDs. A token is held only as its SHA-256 hash.
CREATE TABLE git_repository (
    id UUID PRIMARY KEY,
    remote_hash TEXT NOT NULL UNIQUE,
    last_commit_sha TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE git_access_token (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL REFERENCES person(id) ON DELETE CASCADE,
    project_id UUID REFERENCES project(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    scope TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

-- Migration: 20261021160000_create_mirror_backfill.sql
-- Operator runs that copy SurrealDB rows the PostgreSQL write mirror never
-- saw, in bounded batches. Born in PostgreSQL. A run resumes by its id; each
-- mirror group's checkpoint records the batches written, and a failure names
-- its row by table and UUID only, with a sanitized reason.
CREATE TABLE mirror_backfill_run (
    id UUID PRIMARY KEY,
    batch_size INTEGER NOT NULL CHECK (batch_size > 0),
    status TEXT NOT NULL CHECK (status IN ('running', 'completed')),
    completed_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((status = 'completed') = (completed_at IS NOT NULL))
);

CREATE TABLE mirror_backfill_checkpoint (
    run_id UUID NOT NULL REFERENCES mirror_backfill_run(id) ON DELETE CASCADE,
    mirror TEXT NOT NULL CHECK (mirror <> ''),
    batches INTEGER NOT NULL DEFAULT 0 CHECK (batches >= 0),
    applied INTEGER NOT NULL DEFAULT 0 CHECK (applied >= 0),
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    completed_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, mirror)
);

CREATE TABLE mirror_backfill_failure (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES mirror_backfill_run(id) ON DELETE CASCADE,
    mirror TEXT NOT NULL CHECK (mirror <> ''),
    row_table TEXT NOT NULL CHECK (row_table <> ''),
    row_id TEXT,
    reason TEXT NOT NULL CHECK (reason <> ''),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mirror_backfill_failure_run ON mirror_backfill_failure (run_id, mirror);

-- Migration: 20261021170000_create_blog_video.sql
CREATE TABLE blog_video (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    object_key TEXT NOT NULL UNIQUE,
    width BIGINT NOT NULL CHECK (width > 0),
    height BIGINT NOT NULL CHECK (height > 0),
    orientation TEXT NOT NULL CHECK (orientation IN ('vertical', 'landscape', 'square')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Migration: 20261022120000_create_source_domain_records.sql
-- Full-row PostgreSQL destinations for persistent Surreal source records.

-- Direct record links retain UUIDs; polymorphic links and relation endpoints retain table tags as JSONB.

-- These link columns have no PostgreSQL FK so source orphans remain importable and are reported separately.

CREATE TABLE "authority_use" (
    "authority_id" UUID NOT NULL,
    "disposition" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "position" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "role" TEXT,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "cap_table_holding" (
    "holder_id" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "share_class" TEXT NOT NULL,
    "shares" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "vesting_note" TEXT
);

CREATE TABLE "case" (
    "caption" TEXT NOT NULL,
    "docket_number" TEXT,
    "forum" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "judge" TEXT,
    "jurisdiction" TEXT,
    "posture" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "case_docket_entry" (
    "case_id" UUID NOT NULL,
    "document_asset_id" UUID,
    "entry_number" TEXT NOT NULL,
    "filed_or_served_on" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "integrity_checked_at" TIMESTAMPTZ,
    "integrity_matched" BOOLEAN,
    "integrity_source_sha256_hex" TEXT,
    "kind" TEXT NOT NULL,
    "notation_id" UUID,
    "party" TEXT,
    "scheduled_on" TIMESTAMPTZ,
    "stamp_case_number" TEXT,
    "stamp_entry_number" TEXT,
    "stamp_filed_on" TEXT,
    "supersedes" UUID,
    "title" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "citation" (
    "authority_use_id" UUID NOT NULL,
    "draft_pin" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "quote" TEXT NOT NULL,
    "source_pin" JSONB NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "why" TEXT NOT NULL
);

CREATE TABLE "contract_review" (
    "asset_id" UUID,
    "findings" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "playbook_id" UUID NOT NULL,
    "risk_summary" TEXT,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "deployment_secret_change" (
    "actor_id" TEXT NOT NULL,
    "ciphertext" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "key" TEXT NOT NULL,
    "kms_key_version" TEXT NOT NULL,
    "request_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "wrapped_key" TEXT NOT NULL
);

CREATE TABLE "disclosure" (
    "entity_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "project_id" UUID,
    "summary" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "discovery_item" (
    "discovery_request_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "item_number" BIGINT NOT NULL,
    "objections" TEXT,
    "request_text" TEXT NOT NULL,
    "response_text" TEXT,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "discovery_request" (
    "case_id" UUID NOT NULL,
    "device" TEXT NOT NULL,
    "docket_entry_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "propounding_party" TEXT NOT NULL,
    "responding_party" TEXT NOT NULL,
    "responses_due_on" TEXT,
    "served_on" TEXT,
    "set_number" BIGINT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "document_comment" (
    "anchor_end" BIGINT NOT NULL,
    "anchor_start" BIGINT NOT NULL,
    "body" TEXT NOT NULL,
    "communication_id" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "person_id" UUID NOT NULL,
    "quoted_text" TEXT NOT NULL,
    "resolved" BOOLEAN NOT NULL,
    "review_document_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "efiling_transactions" (
    "accepted_on" TEXT,
    "case_number" TEXT NOT NULL,
    "conformed_document_id" UUID,
    "court" TEXT NOT NULL,
    "efsp" TEXT NOT NULL,
    "filed_on" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "needs_entered_order" BOOLEAN NOT NULL,
    "note" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "submitted_document_id" UUID,
    "transaction_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "estate_asset" (
    "category" TEXT NOT NULL,
    "description" TEXT NOT NULL,
    "funding_state" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "passes_by" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "expunge_record" (
    "authorized_by_person_id" UUID NOT NULL,
    "category" TEXT NOT NULL,
    "head_after" TEXT,
    "head_before" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "path" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "expunge_request" (
    "asset_id" UUID NOT NULL,
    "expunge_record_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "project_id" UUID NOT NULL,
    "requested_by_person_id" UUID NOT NULL,
    "resolved_by_person_id" UUID,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "fiduciary_role" (
    "fiduciary_id" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID,
    "project_id" UUID NOT NULL,
    "role" TEXT NOT NULL,
    "successor_order" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "filing" (
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "notation_id" UUID NOT NULL,
    "office" TEXT NOT NULL,
    "reference" TEXT,
    "submitted_at" TIMESTAMPTZ NOT NULL,
    "summary" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "finance_aggregate" (
    "computed_at" TIMESTAMPTZ NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "parameters" JSONB NOT NULL,
    "project_id" UUID NOT NULL,
    "report_json" TEXT NOT NULL,
    "run_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "firm_anchor" (
    "entity_id" JSONB NOT NULL,
    "id" TEXT NOT NULL,
    "id_kind" TEXT NOT NULL CHECK ("id_kind" IN ('string', 'uuid')),
    PRIMARY KEY ("id_kind", "id")
);

CREATE TABLE "iolta_account" (
    "balance_cents" BIGINT NOT NULL,
    "currency" TEXT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "jurisdiction_id" UUID NOT NULL,
    "mirrored_at" TIMESTAMPTZ,
    "name" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_account_code" TEXT,
    "xero_account_id" TEXT NOT NULL
);

CREATE TABLE "iolta_allocation" (
    "amount_cents" BIGINT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "invoice_reference" TEXT NOT NULL,
    "occurred_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "withdrawal_id" TEXT NOT NULL,
    "xero_invoice_id" TEXT NOT NULL
);

CREATE TABLE "iolta_withdrawal" (
    "currency" TEXT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "jurisdiction_id" UUID NOT NULL,
    "occurred_at" TIMESTAMPTZ NOT NULL,
    "total_cents" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_transaction_id" TEXT NOT NULL
);

CREATE TABLE "notarization" (
    "asset_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notarized_at" TIMESTAMPTZ,
    "notary_person_id" UUID,
    "notation_id" UUID NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_document" (
    "current_version_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "original_asset_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_document_comment" (
    "anchor" TEXT NOT NULL,
    "body" TEXT NOT NULL,
    "decision" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "person_id" UUID NOT NULL,
    "resolved" BOOLEAN NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "version_id" UUID NOT NULL
);

CREATE TABLE "notation_document_version" (
    "anchor_manifest_asset_id" UUID NOT NULL,
    "authored_by_person_id" UUID NOT NULL,
    "document_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "markdown_asset_id" UUID NOT NULL,
    "parent_version_id" UUID,
    "parser_version" TEXT NOT NULL,
    "schema_version" BIGINT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_meter_entry" (
    "currency" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID,
    "package_id" UUID,
    "project_id" UUID NOT NULL,
    "quantity" BIGINT NOT NULL,
    "rate_id" UUID NOT NULL,
    "state" TEXT NOT NULL,
    "unit_amount_cents" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "relationship" (
    "confidence_pct" BIGINT NOT NULL,
    "detail" TEXT,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "source_id" JSONB,
    "source_kind" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "in" JSONB NOT NULL,
    "out" JSONB NOT NULL
);

CREATE TABLE "review_document" (
    "body_html" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "notation_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "title" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "signature" (
    "completed_signer_roles" JSONB,
    "field" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "signed_at" TIMESTAMPTZ,
    "signer_person_id" UUID,
    "state" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "social_oauth_attempt" (
    "actor_person_id" UUID NOT NULL,
    "brand_key" TEXT NOT NULL,
    "consumed_at" TIMESTAMPTZ,
    "expires_at" TIMESTAMPTZ NOT NULL,
    "firm_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "instagram_account_id" TEXT,
    "pkce_verifier" TEXT NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_account_id" TEXT,
    "state_hash" TEXT NOT NULL
);

CREATE TABLE "verification" (
    "citation_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "revision_sha" TEXT NOT NULL,
    "status_citation" TEXT NOT NULL,
    "status_proposition" TEXT NOT NULL,
    "status_quote" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "verifier_person_id" UUID NOT NULL
);

CREATE TABLE "visitor_route_count" (
    "bucket_date" TIMESTAMPTZ NOT NULL,
    "country_code" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "locale" TEXT NOT NULL,
    "route_pattern" TEXT NOT NULL,
    "source" TEXT NOT NULL,
    "status_class" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "visits" BIGINT NOT NULL
);

CREATE TABLE "xero_invoice" (
    "amount_cents" BIGINT NOT NULL,
    "amount_paid_cents" BIGINT NOT NULL,
    "currency" TEXT NOT NULL,
    "due_at" TIMESTAMPTZ,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "issued_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "reference" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_invoice_id" TEXT NOT NULL
);

-- Migration: 20261022140000_create_dbos_workflow_request.sql
-- Durable application-owned request identity for authenticated DBOS ingress.
-- The engine's global workflow ID is reserved for one typed request forever;
-- retries may reuse that request, while a different payload is rejected.
CREATE TABLE dbos_workflow_request (
    workflow_id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Migration: 20261022200000_add_tenancy_authority_constraints.sql
-- Keep PostgreSQL's write authority within the invariants already enforced by
-- SurrealDB. Index creation is intentionally fail-closed if imported rows
-- contain collisions; operators must resolve those rows before cutover.
DO $$
DECLARE
    firm_entity_groups BIGINT;
    brand_name_groups BIGINT;
    invalid_font_licences BIGINT;
BEGIN
    SELECT count(*) INTO firm_entity_groups FROM (
        SELECT entity_id FROM firm WHERE entity_id IS NOT NULL
        GROUP BY entity_id HAVING count(*) > 1
    ) collisions;
    SELECT count(*) INTO brand_name_groups FROM (
        SELECT name FROM brand GROUP BY name HAVING count(*) > 1
    ) collisions;
    SELECT count(*) INTO invalid_font_licences FROM brand
        WHERE font_licence IS NOT NULL AND font_licence NOT IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0');
    IF firm_entity_groups > 0 OR brand_name_groups > 0 OR invalid_font_licences > 0 THEN
        RAISE EXCEPTION 'tenancy constraint preflight failed: firm entity duplicate groups %, brand name duplicate groups %, invalid font licence rows %',
            firm_entity_groups, brand_name_groups, invalid_font_licences;
    END IF;
END $$;
CREATE UNIQUE INDEX firm_entity ON firm (entity_id) WHERE entity_id IS NOT NULL;
CREATE UNIQUE INDEX brand_name ON brand (name);
ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));

-- Migration: 20261022210000_enforce_project_code.sql
-- Project routes, repository bindings and document prefixes share this coordinate.
-- Keep the source's READONLY and reserved-route rules in the database itself.
ALTER TABLE project ADD CONSTRAINT project_code_reserved CHECK (code NOT IN ('new', 'closed'));

CREATE FUNCTION project_code_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code THEN
        RAISE EXCEPTION 'Project code is immutable'
            USING ERRCODE = '23514', CONSTRAINT = 'project_code_immutable';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER project_code_immutable BEFORE UPDATE OF code ON project
    FOR EACH ROW EXECUTE FUNCTION project_code_immutable();

-- Migration: 20261025120000_add_project_deadline_title.sql
-- A Deadline's short title: what is due. NULL for rows written before titles existed.
ALTER TABLE project_deadlines ADD COLUMN title TEXT CHECK (title IS NULL OR length(btrim(title)) BETWEEN 1 AND 200);

-- Migration: 20261026120000_add_deadline_calendar_sequence.sql
ALTER TABLE project_deadlines ADD COLUMN calendar_sequence BIGINT CHECK (calendar_sequence IS NULL OR calendar_sequence BETWEEN 0 AND 2147483647);

-- Migration: 20261026130000_add_deadline_description.sql
ALTER TABLE project_deadlines ADD COLUMN description TEXT;

-- Migration: 20261027120000_add_project_calendar_and_booking_url.sql
-- A Project's shared Google Calendar id and its client booking link. NULL until recorded.
ALTER TABLE project ADD COLUMN calendar_id TEXT;
ALTER TABLE project ADD COLUMN booking_url TEXT;

-- Migration: 20261029010000_add_communication_receipts.sql
ALTER TABLE communication ADD COLUMN counterparty_role TEXT CHECK (counterparty_role IN ('opposing_counsel', 'client', 'court', 'other'));
ALTER TABLE communication ADD COLUMN acknowledges UUID REFERENCES communication(id) ON DELETE SET NULL;
CREATE INDEX communication_acknowledges ON communication (project_id, acknowledges);

-- Migration: 20261030010000_add_template_firm.sql
-- A Firm Library version: the Firm whose library repository the Template was synced from.
ALTER TABLE template ADD COLUMN firm_id UUID REFERENCES firm(id);
ALTER TABLE template ADD CONSTRAINT template_one_scope CHECK (project_id IS NULL OR firm_id IS NULL);

-- Migration: 20261030020000_add_asset_evidence.sql
-- The exhibit metadata the matter page's evidence index reads (LAW-185):
-- `{ dated_at, source, confidence, supports }` as a JSON object. NULL for
-- rows written before exhibits carried provenance; the shape is
-- `store::documents::Evidence`, validated before the write.
ALTER TABLE asset ADD COLUMN evidence JSONB;

-- Migration: 20261030030000_create_inbox_message_asset.sql
-- SurrealDB remains the write authority; Inbox rows are opt-in mirrors.
CREATE TABLE inbox_message_asset (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    message_id UUID NOT NULL REFERENCES message(id),
    asset_id UUID NOT NULL REFERENCES asset(id),
    filename TEXT NOT NULL,
    position INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT inbox_message_asset_key UNIQUE (message_id, sha256)
);
CREATE INDEX inbox_message_asset_message ON inbox_message_asset (message_id);

CREATE TRIGGER inbox_message_asset_immutable
    BEFORE UPDATE OR DELETE ON inbox_message_asset
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();

-- Migration: 20261030120000_create_project_task.sql
-- Net-new matter work tracking: assignable Project tasks with audited state
-- transitions. A Task is work someone does, with a workflow state; it is not a
-- Deadline (a docket obligation the date binds). See docs/glossary/task.md.
-- PostgreSQL-native, following the project_costs pattern: retained rows,
-- trigger-enforced audit events, immutable event log.
--
-- The composite foreign key below pins an assignee to a Person who already
-- holds a participation row on the same Project. A UNIQUE (project_id,
-- person_id) index is required for it, in the FK's column order.
CREATE UNIQUE INDEX person_project_role_project_person
    ON person_project_role (project_id, person_id);

CREATE TABLE project_task (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    title TEXT NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    body TEXT CHECK (body IS NULL OR length(btrim(body)) BETWEEN 1 AND 2000),
    assignee_id UUID,
    due_on DATE,
    state TEXT NOT NULL CHECK (state IN ('todo', 'in_progress', 'done')),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, assignee_id)
        REFERENCES person_project_role (project_id, person_id)
);
CREATE INDEX project_task_project ON project_task (project_id, state, due_on, id);
CREATE INDEX project_task_assignee ON project_task (assignee_id);

CREATE TABLE project_task_events (
    id UUID PRIMARY KEY,
    project_task_id UUID NOT NULL REFERENCES project_task(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'assignee_changed', 'state_changed')),
    before_state JSONB,
    after_state JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_task_events_task_time ON project_task_events (project_task_id, occurred_at, id);

CREATE FUNCTION enforce_project_task_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Project tasks are retained; a state transition, never a delete';
    END IF;
    actor_id := nullif(current_setting('navigator.project_task_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.project_task_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('assignee_changed', 'state_changed') THEN
        RAISE EXCEPTION 'Project task updates require an actor and audit event';
    END IF;
    INSERT INTO project_task_events(id,project_task_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_task_audit_update
    BEFORE UPDATE OR DELETE ON project_task
    FOR EACH ROW EXECUTE FUNCTION enforce_project_task_audit();

CREATE FUNCTION record_project_task_creation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
BEGIN
    actor_id := nullif(current_setting('navigator.project_task_actor', true), '')::UUID;
    IF actor_id IS NULL OR current_setting('navigator.project_task_event_kind', true) IS DISTINCT FROM 'created' THEN
        RAISE EXCEPTION 'Project task creation requires an actor and audit event';
    END IF;
    INSERT INTO project_task_events(id,project_task_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,'created',NULL,to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_task_audit_create
    AFTER INSERT ON project_task
    FOR EACH ROW EXECUTE FUNCTION record_project_task_creation();

CREATE FUNCTION reject_project_task_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Project task audit events are immutable';
END;
$$;
CREATE TRIGGER project_task_events_immutable
    BEFORE UPDATE OR DELETE ON project_task_events
    FOR EACH ROW EXECUTE FUNCTION reject_project_task_event_mutation();

-- Migration: 20261031010000_create_navigator_graph.sql
-- Migration: 20261031010000_create_navigator_graph.sql
-- The Apache AGE relationship graph, projected from the relational mirrors.
--
-- `CREATE EXTENSION age` normally runs earlier, in the superuser init path
-- (k8s/postgres/access/roles.sql); this migration owns the graph itself so the
-- migration login owns every graph object. Environments that apply the
-- canonical schema directly with no privileged init step (CI fixtures, scratch
-- databases) bootstrap the extension in the block below under IF-NOT-EXISTS
-- semantics: a no-op when the init path already created it, and a loud
-- permission error for a non-superuser login when the init path never ran.
-- The loader helpers are the only
-- Cypher entry points for backfill batches: AGE's three-argument cypher()
-- accepts a bound parameter only as a bare Param node, so each helper takes
-- one JSON text payload, parses it into an agtype once, and passes it as a
-- PL/pgSQL variable — which the engine substitutes as exactly that Param.
-- The Cypher in each branch is a fixed dollar-quoted constant: no value ever
-- becomes query text.

-- The extension bootstrap lives in a DO block because the canonical-SQL
-- parser tracks procedural blocks as opaque and every bare statement it does
-- not know is rejected; the guard follows for the same reason.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'age') THEN
        CREATE EXTENSION age;
    END IF;
END
$$;

-- The guard lives in a DO block because the canonical-SQL parser tracks
-- procedural blocks as opaque and every bare statement it does not know
-- is rejected; the migration login owns the graph either way.
DO $$
DECLARE
    graph_id oid;
    label text;
BEGIN
    SELECT graphid INTO graph_id
    FROM ag_catalog.ag_graph WHERE ag_catalog.ag_graph.name = 'navigator_graph';
    IF graph_id IS NULL THEN
        PERFORM ag_catalog.create_graph('navigator_graph');
        graph_id := (
            SELECT graphid FROM ag_catalog.ag_graph
            WHERE ag_catalog.ag_graph.name = 'navigator_graph'
        );
    END IF;
    -- Every label exists before any load: validation and the property
    -- indexes read the label tables even when a family has no rows yet,
    -- and a label created lazily by the first batch would not exist on an
    -- empty projection.
    FOREACH label IN ARRAY ARRAY['Person', 'Entity', 'Project', 'Firm'] LOOP
        IF NOT EXISTS (
            SELECT 1 FROM ag_catalog.ag_label
            WHERE ag_catalog.ag_label.graph = graph_id AND ag_catalog.ag_label.name = label
        ) THEN
            PERFORM ag_catalog.create_vlabel('navigator_graph', label::cstring);
        END IF;
    END LOOP;
    FOREACH label IN ARRAY ARRAY[
        'HAS_ROLE', 'RELATIONSHIP', 'WORKS_ON', 'MEMBER_OF', 'HANDLES'
    ] LOOP
        IF NOT EXISTS (
            SELECT 1 FROM ag_catalog.ag_label
            WHERE ag_catalog.ag_label.graph = graph_id AND ag_catalog.ag_label.name = label
        ) THEN
            PERFORM ag_catalog.create_elabel('navigator_graph', label::cstring);
        END IF;
    END LOOP;
END
$$;

-- Runtime read access: cypher() executes as the calling login, so the app
-- needs USAGE on the graph's schema and SELECT on its label tables; writes
-- stay with the migration login through the loader helpers. The grant is
-- guarded because environments that apply the canonical schema directly
-- (CI fixtures) have no navigator_app role to grant to, and skipped when the
-- role is absent rather than failing the schema application.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        GRANT USAGE ON SCHEMA navigator_graph TO navigator_app;
        GRANT SELECT ON ALL TABLES IN SCHEMA navigator_graph TO navigator_app;
        ALTER DEFAULT PRIVILEGES FOR ROLE navigator_owner IN SCHEMA navigator_graph
            GRANT SELECT ON TABLES TO navigator_app;
    END IF;
END
$$;

-- Vertex batches. `kind` selects the fixed label; ids are the Surreal record
-- UUIDs, so relationship source_id chains and mirror parity stay traceable.
CREATE OR REPLACE FUNCTION graph_load_vertices(kind text, rows text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := rows::agtype;
BEGIN
    IF kind = 'person' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Person {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'entity' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Entity {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'project' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Project {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'firm' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            CREATE (:Firm {id: row.id, name: row.name})
        $Q$, payload) AS (v agtype);
    ELSE
        RAISE EXCEPTION 'unknown graph vertex kind %', kind;
    END IF;
END
$fn$;

-- Edge batches. Endpoints match by vertex id, so a batch may reference
-- vertices loaded in an earlier batch of the same transaction.
CREATE OR REPLACE FUNCTION graph_load_edges(kind text, rows text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := rows::agtype;
BEGIN
    IF kind = 'has_role' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:HAS_ROLE {role: row.role}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'relationship' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:RELATIONSHIP {kind: row.kind, confidence_pct: row.confidence_pct,
                                       source_kind: row.source_kind, source_id: row.source_id,
                                       detail: row.detail}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'works_on' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:WORKS_ON {role: row.role}]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'member_of' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:MEMBER_OF]->(b)
        $Q$, payload) AS (v agtype);
    ELSIF kind = 'handles' THEN
        RETURN QUERY
        SELECT * FROM cypher('navigator_graph', $Q$
            UNWIND $rows AS row
            MATCH (a), (b) WHERE a.id = row.from_id AND b.id = row.to_id
            CREATE (a)-[:HANDLES]->(b)
        $Q$, payload) AS (v agtype);
    ELSE
        RAISE EXCEPTION 'unknown graph edge kind %', kind;
    END IF;
END
$fn$;

-- Full projection rebuild: the graph arm clears and reloads inside one
-- transaction, so a run leaves either the old graph or the new one.
CREATE OR REPLACE FUNCTION graph_clear_navigator()
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$ MATCH (n) DETACH DELETE n $Q$)
        AS (v agtype);
END
$fn$;

-- One frontier hop of the conflict walk, both conflict edge types. AGE 1.6.0
-- rejects multi-type edge patterns (`[:RELATIONSHIP|HAS_ROLE]`), so each type
-- is its own fixed query and the results union in PL/pgSQL; the frontier
-- arrives as a bound parameter (an agtype object with a `frontier` id array),
-- never as query text.
CREATE OR REPLACE FUNCTION graph_conflict_frontier(frontier text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := frontier::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:RELATIONSHIP]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'RELATIONSHIP', kind: r.kind, role: null,
                         confidence_pct: r.confidence_pct, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r))}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r))}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- The bounded conflict walk: every conflict edge within 3 hops of the anchors,
-- expanded one frontier at a time exactly as the Surreal conflict traversal is
-- (store/src/conflicts.rs `MAX_HOPS` = 3; keep the two bounds in step). The
-- unrestricted variable-length walk is not used here: it would cross
-- participation edges (WORKS_ON, MEMBER_OF, HANDLES) that Surreal's walk never
-- follows, so the graph arm's parity check would disagree with the source
-- engine. Cross-hop duplicates are deduplicated by the caller.
CREATE OR REPLACE FUNCTION graph_conflict_edges(anchors text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    frontier jsonb := anchors::jsonb;
    round_edges jsonb;
    all_edges jsonb := '[]'::jsonb;
BEGIN
    FOR hop IN 1..3 LOOP
        EXIT WHEN jsonb_array_length(frontier) = 0;
        SELECT coalesce(jsonb_agg(edge_obj), '[]'::jsonb)
        INTO round_edges
        FROM (
            SELECT (edge::text)::jsonb AS edge_obj
            FROM graph_conflict_frontier('{"frontier": ' || frontier::text || '}') AS edge
        ) rounds;
        all_edges := all_edges || round_edges;
        SELECT jsonb_agg(DISTINCT endpoint)
        INTO frontier
        FROM (
            SELECT edge->>'from_id' AS endpoint
            FROM jsonb_array_elements(round_edges) AS edge
            UNION
            SELECT edge->>'to_id'
            FROM jsonb_array_elements(round_edges) AS edge
        ) endpoints;
    END LOOP;
    RETURN QUERY
    SELECT (edge::text)::agtype
    FROM jsonb_array_elements(all_edges) AS edge;
END
$fn$;

-- Loaders are the migration login's write path; the runtime login runs
-- read-only Cypher of its own and never executes these.
REVOKE EXECUTE ON FUNCTION graph_load_vertices(text, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_load_edges(text, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_clear_navigator() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_conflict_frontier(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_conflict_edges(text) FROM PUBLIC;

-- Migration: 20261031020000_create_graph_view_readers.sql
-- Migration: 20261031020000_create_graph_view_readers.sql
-- Read-only reader functions for the graph views: bounded neighborhood
-- expansion (project, lawyer) and the firm-to-firm aggregation. These are
-- the Cypher entry points for the graph REST endpoints, the same pattern
-- as the conflict readers in 20261031010000_create_navigator_graph.sql:
-- each helper takes one JSON text payload, parses it into an agtype once,
-- and passes it as a PL/pgSQL variable. The Cypher in each branch is a
-- fixed dollar-quoted constant: no value ever becomes query text.
--
-- AGE 1.6.0 constraints honored here (docs/age-cypher-parity-probe.md):
-- no multi-type edge patterns — every label is matched in its own query,
-- hop by hop; no order-by on a Cypher alias (ordering happens SQL-side or
-- in Rust); `id(e)`/`label(n)` rather than property access.

-- The seed vertex for a view: id, label, and name. An absent vertex means
-- the graph projection has not been run yet — callers render an empty
-- neighborhood, not an error.
CREATE OR REPLACE FUNCTION graph_view_seed(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)
        WHERE n.id = $seed
        RETURN DISTINCT {id: n.id, label: label(n), name: n.name}
    $Q$, payload) AS (vertex agtype);
END
$fn$;

-- Hop 1 of the project view: everything hanging directly off the project
-- vertex — the parties (WORKS_ON, undirected) and the firms whose members
-- participate on it (HANDLES, directed at the project).
CREATE OR REPLACE FUNCTION graph_project_attach(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)-[r:WORKS_ON]-(p:Project)
        WHERE p.id = $seed
        RETURN DISTINCT {edge_id: id(r), edge_type: 'WORKS_ON', kind: null, role: r.role,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (n)-[r:HANDLES]->(p:Project)
        WHERE p.id = $seed
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HANDLES', kind: null, role: null,
                         confidence_pct: null, from_id: startNode(r).id,
                         from_label: label(startNode(r)), to_id: endNode(r).id,
                         to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- Hops 2+ of the project view: one frontier step outward from the already
-- reached persons, entities, and firms. Only persons expand — relationships
-- and entity roles anchor on persons, and a firm's participation is already
-- materialized as its HANDLES edges. Because each hop passes its own
-- frontier, no vertex is expanded twice (same bounded walk as the conflict
-- readers).
CREATE OR REPLACE FUNCTION graph_project_expand(frontier text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := frontier::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:RELATIONSHIP]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'RELATIONSHIP', kind: r.kind, role: null,
                         confidence_pct: r.confidence_pct, source_kind: r.source_kind,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:MEMBER_OF]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'MEMBER_OF', kind: null, role: null,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- Every hop of the lawyer view: everything around the lawyer-person seed
-- and then one frontier step outward. Persons expand through their
-- relationships, entity roles, memberships, and participations; reached
-- projects and firms are endpoint-only (only persons expand — the project
-- view owns project-rooted neighborhoods).
CREATE OR REPLACE FUNCTION graph_lawyer_expand(frontier text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := frontier::agtype;
BEGIN
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:RELATIONSHIP]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'RELATIONSHIP', kind: r.kind, role: null,
                         confidence_pct: r.confidence_pct, source_kind: r.source_kind,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:HAS_ROLE]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'HAS_ROLE', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:MEMBER_OF]->(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'MEMBER_OF', kind: null, role: null,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
    RETURN QUERY
    SELECT * FROM cypher('navigator_graph', $Q$
        MATCH (a)-[r:WORKS_ON]-(x)
        WHERE a.id IN $frontier
        RETURN DISTINCT {edge_id: id(r), edge_type: 'WORKS_ON', kind: null, role: r.role,
                         confidence_pct: null, source_kind: null,
                         from_id: startNode(r).id, from_label: label(startNode(r)),
                         to_id: endNode(r).id, to_label: label(endNode(r)),
                         from_name: startNode(r).name,
                         to_name: endNode(r).name}
    $Q$, payload) AS (edge agtype);
END
$fn$;

-- The firm view: person-level edges aggregated up to firm-firm ties for the
-- seed firm. Every counterpart firm in a person's edge sits three hops away
-- (member -> person-level edge -> member's firm), so this is one aggregation
-- read, not a frontier walk — depth-independent by construction.
--
-- kind values mirror the lens a viewer reads: adversary (an adverse_to
-- relationship between members), co_counsel (members firm-side on the same
-- matter), of_counsel (any other cross-firm relationship between members),
-- client (a counterparty-firm member client-side on a matter this firm's
-- members participate firm-side). strength counts the distinct person-level edges
-- behind the tie.
CREATE OR REPLACE FUNCTION graph_firm_view(seed text)
RETURNS SETOF ag_catalog.agtype
LANGUAGE plpgsql
SET search_path = public, ag_catalog
AS $fn$
DECLARE
    payload agtype := seed::agtype;
BEGIN
    -- Each arm returns plain grouping columns (AGE rejects a RETURN map that
    -- mixes grouping keys with aggregates) and the SELECT wraps them into one
    -- JSON map per tie. The `(x::text)::jsonb` round-trip re-homes an agtype
    -- value into the JSON object verbatim.
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'adversary',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:RELATIONSHIP]-(b:Person)
        WHERE r.kind = 'adverse_to'
        MATCH (b)-[:MEMBER_OF]->(fb:Firm)
        WHERE fb.id <> fa.id
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT r) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'co_counsel',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:WORKS_ON]->(p:Project)
        WHERE r.role <> 'client'
        MATCH (m2:Person)-[r2:WORKS_ON]->(p)
        MATCH (m2)-[:MEMBER_OF]->(fb:Firm)
        WHERE m2 <> a AND fb.id <> fa.id AND r2.role <> 'client'
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT p) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'of_counsel',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:RELATIONSHIP]-(b:Person)
        WHERE r.kind <> 'adverse_to'
        MATCH (b)-[:MEMBER_OF]->(fb:Firm)
        WHERE fb.id <> fa.id
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT r) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
    RETURN QUERY
    SELECT jsonb_build_object(
        'other_firm_id', t.other_id::text,
        'other_firm_name', t.other_name::text,
        'kind', 'client',
        'strength', (t.strength::text)::bigint)::text::agtype
    FROM cypher('navigator_graph', $Q$
        MATCH (a:Person)-[:MEMBER_OF]->(fa:Firm)
        WHERE fa.id = $seed
        MATCH (a)-[r:WORKS_ON]->(p:Project)
        WHERE r.role <> 'client'
        MATCH (c:Person)-[r2:WORKS_ON]->(p)
        MATCH (c)-[:MEMBER_OF]->(fb:Firm)
        WHERE c <> a AND fb.id <> fa.id AND r2.role = 'client'
        RETURN fb.id AS other_id, fb.name AS other_name, count(DISTINCT c) AS strength
    $Q$, payload) AS t(other_id agtype, other_name agtype, strength agtype);
END
$fn$;

-- View readers are the runtime login's only graph entry points, like the
-- conflict readers before them: revoking PUBLIC and granting navigator_app
-- when the role exists (guarded, because role-less CI fixtures skip it).
REVOKE EXECUTE ON FUNCTION graph_view_seed(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_project_attach(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_project_expand(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_lawyer_expand(text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION graph_firm_view(text) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        GRANT EXECUTE ON FUNCTION graph_view_seed(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_project_attach(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_project_expand(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_lawyer_expand(text) TO navigator_app;
        GRANT EXECUTE ON FUNCTION graph_firm_view(text) TO navigator_app;
    END IF;
END
$$;

-- Migration: 20261031120000_add_project_onboarding_jurisdiction.sql
-- The jurisdiction an onboarding Notation's Clauses bind to. NULL until recorded.
ALTER TABLE project ADD COLUMN onboarding_jurisdiction TEXT;

-- Migration: 20261031130000_add_deadline_category.sql
-- Deadline categories: what kind of date the deadline is, rendered by the
-- matter calendar's fixed category-to-color map. Nullable: rows written
-- before categories existed read back without one.
ALTER TABLE project_deadlines ADD COLUMN category TEXT CHECK (category IN ('court', 'hearing', 'discovery', 'internal'));

