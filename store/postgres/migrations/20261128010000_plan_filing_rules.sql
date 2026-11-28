-- The Firm supplies its own dated filing rules for each covered benefit.
CREATE TABLE legal_plan_filing_rule (
    legal_plan_id UUID NOT NULL REFERENCES legal_plan(id) ON DELETE CASCADE,
    benefit_code TEXT NOT NULL CHECK (benefit_code <> '' AND benefit_code = btrim(benefit_code)),
    effective_on DATE NOT NULL,
    trigger TEXT NOT NULL CHECK (trigger IN ('service_completed', 'hours_exhausted', 'matter_closed', 'hours_exhausted_or_matter_closed')),
    filing_window_days BIGINT CHECK (filing_window_days BETWEEN 1 AND 4294967295),
    filing_destination TEXT NOT NULL CHECK (btrim(filing_destination) <> ''),
    filer TEXT NOT NULL CHECK (btrim(filer) <> ''),
    PRIMARY KEY (legal_plan_id, benefit_code, effective_on)
);

ALTER TABLE project ADD COLUMN plan_member_rate_cents_per_hour INTEGER;
ALTER TABLE project ADD CONSTRAINT project_plan_member_rate_check CHECK (plan_member_rate_cents_per_hour IS NULL OR (plan_cap_shape IS NOT NULL AND plan_member_rate_cents_per_hour >= 0));
