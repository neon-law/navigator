-- Attorney fee structures: effective-dated hourly rates and one flat fee per
-- matter. An attorney_rate row prices one attorney's hour either firm-wide
-- (project_id IS NULL) or on one matter (the override). Rates are
-- effective-dated and never edited — a change inserts a row with a new
-- effective_on, so the table is its own history. Captured Time Entries are
-- immune by design: they snapshot rate_cents_per_hour at creation
-- (docs/glossary/fee-structure.md, docs/glossary/time-entry.md).
CREATE TABLE attorney_rate (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id uuid NOT NULL REFERENCES person (id) ON DELETE CASCADE,
    project_id uuid REFERENCES project (id) ON DELETE CASCADE,
    hourly_rate_cents integer NOT NULL CHECK (hourly_rate_cents >= 0),
    effective_on date NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now()
);

-- One rate per attorney per scope per effective date: the matter scope and
-- the firm-default scope are each unique (partial indexes, because NULL
-- project_id never equals anything in a plain unique constraint).
CREATE UNIQUE INDEX attorney_rate_matter_effective
    ON attorney_rate (person_id, project_id, effective_on) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX attorney_rate_firm_effective
    ON attorney_rate (person_id, effective_on) WHERE project_id IS NULL;

-- Rate lookups by attorney and effective date (both scopes).
CREATE INDEX attorney_rate_person_idx ON attorney_rate (person_id, effective_on);

-- The rate table is append-only history: a correction is a new effective
-- date, never an edit in place. Deleting a mistyped row is permitted —
-- captured Time Entries carry their own snapshot and are unaffected.
CREATE FUNCTION reject_attorney_rate_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Attorney rates are effective-dated history; insert a new effective_on instead of updating';
END;
$$;
CREATE TRIGGER attorney_rate_immutable
    BEFORE UPDATE ON attorney_rate
    FOR EACH ROW EXECUTE FUNCTION reject_attorney_rate_update();

-- A matter's current flat fee — the agreed figure, replaceable, not history.
CREATE TABLE matter_flat_fee (
    project_id uuid PRIMARY KEY REFERENCES project (id) ON DELETE CASCADE,
    fee_cents bigint NOT NULL CHECK (fee_cents >= 0),
    inserted_at timestamptz NOT NULL DEFAULT now()
);
