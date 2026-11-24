CREATE TABLE lead_intake (
    id TEXT PRIMARY KEY,
    lead_id UUID NOT NULL REFERENCES lead(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    school_district TEXT NOT NULL,
    relationship TEXT NOT NULL,
    concern TEXT NOT NULL,
    next_date DATE
);
CREATE INDEX lead_intake_history ON lead_intake (lead_id, submitted_at);
