-- A provider case identifies a referral. A certificate identifies coverage.
ALTER TABLE plan_intake ADD COLUMN plan_case_number TEXT;
ALTER TABLE plan_intake ADD COLUMN notice_matter_type TEXT;
ALTER TABLE plan_intake ADD COLUMN take_started_at TIMESTAMPTZ;

CREATE INDEX plan_intake_case
    ON plan_intake (firm_id, provider, lower(plan_case_number))
    WHERE plan_case_number IS NOT NULL;
