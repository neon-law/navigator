-- Keep the amount observed on an accepted envelope separate from a posted Project Cost.
ALTER TABLE efiling_transactions ADD COLUMN fee_receipt_document_id UUID REFERENCES asset(id);
ALTER TABLE efiling_transactions ADD COLUMN fee_amount_cents BIGINT;
ALTER TABLE efiling_transactions ADD COLUMN fee_currency TEXT;
ALTER TABLE efiling_transactions ADD COLUMN fee_cost_id UUID REFERENCES project_costs(id);
ALTER TABLE efiling_transactions ADD COLUMN fee_source_reference TEXT;
ALTER TABLE efiling_transactions ADD COLUMN fee_bank_account_id UUID;
ALTER TABLE efiling_transactions ADD COLUMN fee_matched_by UUID REFERENCES person(id);
ALTER TABLE efiling_transactions ADD COLUMN fee_matched_at TIMESTAMPTZ;
ALTER TABLE efiling_transactions ADD CONSTRAINT efiling_fee_observation_complete CHECK ((fee_receipt_document_id IS NULL AND fee_amount_cents IS NULL AND fee_currency IS NULL) OR (fee_receipt_document_id IS NOT NULL AND fee_amount_cents IS NOT NULL AND fee_amount_cents > 0 AND fee_currency IS NOT NULL AND fee_currency ~ '^[A-Z]{3}$'));
ALTER TABLE efiling_transactions ADD CONSTRAINT efiling_fee_settlement_complete CHECK ((fee_cost_id IS NULL AND fee_source_reference IS NULL AND fee_bank_account_id IS NULL AND fee_matched_by IS NULL AND fee_matched_at IS NULL) OR (fee_cost_id IS NOT NULL AND fee_receipt_document_id IS NOT NULL AND fee_source_reference IS NOT NULL AND fee_bank_account_id IS NOT NULL AND fee_matched_by IS NOT NULL AND fee_matched_at IS NOT NULL));
CREATE UNIQUE INDEX efiling_fee_receipt_once ON efiling_transactions (fee_receipt_document_id) WHERE fee_receipt_document_id IS NOT NULL;
CREATE UNIQUE INDEX efiling_fee_cost_once ON efiling_transactions (fee_cost_id) WHERE fee_cost_id IS NOT NULL;
CREATE UNIQUE INDEX efiling_fee_source_once ON efiling_transactions (fee_source_reference) WHERE fee_source_reference IS NOT NULL;
CREATE INDEX project_costs_source_lookup ON project_costs (source_reference) WHERE source_reference IS NOT NULL;
