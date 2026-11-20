-- A receipt has one durable recovery owner across admission, delivery reopening and child completion.
ALTER TABLE email_receipt ADD COLUMN summary_recovery_operation UUID;
