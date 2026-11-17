ALTER TABLE matter_billing_batch ALTER COLUMN xero_contact_id DROP NOT NULL;
ALTER TABLE matter_billing_batch ADD COLUMN stripe_account_id TEXT;
ALTER TABLE matter_billing_batch ADD COLUMN stripe_iolta BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE matter_billing_batch ADD COLUMN stripe_invoice_id TEXT;
ALTER TABLE matter_billing_batch ADD CONSTRAINT matter_billing_batch_issuer CHECK ((stripe_account_id IS NULL AND NOT stripe_iolta AND stripe_invoice_id IS NULL AND xero_contact_id IS NOT NULL) OR (stripe_account_id IS NOT NULL AND xero_contact_id IS NULL AND xero_invoice_id IS NULL));
