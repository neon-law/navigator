-- A Legal Plan: a panel program the Firm performs work for (ARAG,
-- LegalShield, …), one row per plan the Firm itself works. The vocabulary is
-- Firm data — two Firms on one deployment each hold their own ARAG row with
-- their own contact and service levels — never shared reference data
-- (docs/glossary/legal-plan.md).
CREATE TABLE legal_plan (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    contact TEXT,
    portal TEXT,
    acceptance_sla_hours INTEGER NOT NULL CHECK (acceptance_sla_hours > 0),
    turnaround_sla_days INTEGER NOT NULL CHECK (turnaround_sla_days > 0),
    reporting_requirements TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (firm_id, name)
);
CREATE INDEX legal_plan_firm ON legal_plan (firm_id);
