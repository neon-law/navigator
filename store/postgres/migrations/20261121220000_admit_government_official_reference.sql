ALTER TABLE project_reference DROP CONSTRAINT project_reference_reason;
ALTER TABLE project_reference ADD CONSTRAINT project_reference_reason CHECK (reason IN ('counterparty_officer', 'registered_agent', 'signatory', 'adverse_party', 'counterparty_entity', 'government_official'));
