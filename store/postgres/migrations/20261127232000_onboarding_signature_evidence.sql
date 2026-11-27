CREATE TABLE onboarding_signature_evidence (
    asset_id uuid PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    firm_id uuid NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    provider text NOT NULL,
    provider_request_id text NOT NULL,
    provider_account_email text NOT NULL,
    signed_on date NOT NULL,
    verified_by uuid NOT NULL REFERENCES person(id),
    verified_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_request_id)
);

CREATE INDEX onboarding_signature_evidence_project ON onboarding_signature_evidence(project_id);
