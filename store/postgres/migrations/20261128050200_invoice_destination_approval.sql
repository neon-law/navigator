-- A lawyer's approval to collect one invoice into the Firm's Stripe
-- Operating account instead of trust (ENG-1343). A missing trust route never
-- selects Operating on its own; this row is the audit record of the
-- exception: who approved, when, why, the destination, and — through the
-- invoice or Matter Billing Batch that references it — what for.
CREATE TABLE invoice_destination_approval (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    project_id UUID NOT NULL REFERENCES project(id),
    destination TEXT NOT NULL CHECK (destination = 'operating'),
    reason TEXT NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    -- The reviewed invoice digest the approval was given for.
    preview_digest TEXT NOT NULL CHECK (length(preview_digest) > 0),
    approved_by UUID NOT NULL REFERENCES person(id),
    approved_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX invoice_destination_approval_project ON invoice_destination_approval (project_id, approved_at);

CREATE FUNCTION protect_invoice_destination_approval() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'An invoice destination approval is a retained audit record';
END;
$$;
CREATE TRIGGER invoice_destination_approval_retained BEFORE UPDATE OR DELETE ON invoice_destination_approval
    FOR EACH ROW EXECUTE FUNCTION protect_invoice_destination_approval();

-- An Operating batch names the one approval it collects under; a retry
-- stays pinned to it.
ALTER TABLE matter_billing_batch ADD COLUMN destination_approval_id UUID UNIQUE REFERENCES invoice_destination_approval(id);
ALTER TABLE matter_billing_batch ADD CONSTRAINT matter_billing_batch_operating_approved CHECK (destination_approval_id IS NULL OR (stripe_account_id IS NOT NULL AND NOT stripe_iolta));
