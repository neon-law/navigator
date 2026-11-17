-- The accounting tables hold the application's authority for these rows once
-- NAVIGATOR_POSTGRES_HOST is set, so the unique semantics SurrealDB's schema
-- enforces on the same tables must hold here too: one pooled IOLTA account
-- per state, and one allocation line per (withdrawal, invoice) pair.
CREATE UNIQUE INDEX iolta_account_jurisdiction_key ON iolta_account (jurisdiction_id);
CREATE UNIQUE INDEX iolta_allocation_withdrawal_invoice_key
    ON iolta_allocation (withdrawal_id, xero_invoice_id);
