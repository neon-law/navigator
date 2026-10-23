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
