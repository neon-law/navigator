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
