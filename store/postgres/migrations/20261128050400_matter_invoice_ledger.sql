-- The Matter Invoice ledger (LAW-507). Stripe is the only invoice issuer;
-- money collected outside Stripe (in person, by check or wire) is recorded by
-- hand. Each invoice has its payments as separate rows, so the balance due is
-- the invoice less its payments. Xero no longer issues or mirrors invoices:
-- every `xero_invoice` row becomes a ledger row with `source = 'xero'` and its
-- Xero id kept as `external_id`, and the work, costs, batches and trust
-- allocations that named a Xero invoice now name its ledger row. Nothing is
-- deleted from object storage.
CREATE TABLE matter_invoice (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    source TEXT NOT NULL CHECK (source IN ('stripe', 'in_person', 'check', 'wire', 'other', 'xero')),
    -- The issuer's id: a Stripe invoice id, a check number, a Xero id kept as
    -- history. Absent only on a Stripe draft not yet created.
    external_id TEXT CHECK (external_id IS NULL OR length(btrim(external_id)) BETWEEN 1 AND 200),
    -- What the client sees naming the invoice.
    reference TEXT NOT NULL CHECK (length(btrim(reference)) BETWEEN 1 AND 200),
    status TEXT NOT NULL CHECK (status IN ('pending', 'draft', 'sent', 'paid', 'void', 'uncollectible')),
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    issued_on DATE,
    due_on DATE,
    payer TEXT CHECK (payer IS NULL OR length(btrim(payer)) BETWEEN 1 AND 300),
    memo TEXT CHECK (memo IS NULL OR length(btrim(memo)) BETWEEN 1 AND 2000),
    -- Why a void or uncollectible invoice ended that way.
    status_reason TEXT CHECK (status_reason IS NULL OR length(btrim(status_reason)) BETWEEN 1 AND 1000),
    client_visible BOOLEAN NOT NULL DEFAULT false,
    receipt_document_id UUID REFERENCES asset(id),
    -- A Navigator-composed Stripe invoice: its retry key, frozen lines and
    -- terms, where it collects, and the billed Person.
    request_id UUID,
    composition JSONB,
    destination TEXT CHECK (destination IN ('trust', 'operating')),
    trust_state TEXT CHECK (trust_state IS NULL OR trust_state ~ '^[A-Z]{2}$'),
    stripe_account_id TEXT CHECK (stripe_account_id IS NULL OR stripe_account_id ~ '^acct_[A-Za-z0-9]+$'),
    destination_approval_id UUID UNIQUE REFERENCES invoice_destination_approval(id),
    billed_person_id UUID REFERENCES person(id),
    first_attempted_at TIMESTAMPTZ,
    created_by UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, source, external_id),
    UNIQUE (project_id, request_id),
    CONSTRAINT matter_invoice_issued CHECK (status = 'pending' OR external_id IS NOT NULL OR source <> 'stripe'),
    CONSTRAINT matter_invoice_destination CHECK (
        (destination IS NULL AND destination_approval_id IS NULL)
        OR (destination = 'trust' AND destination_approval_id IS NULL AND stripe_account_id IS NOT NULL)
        OR (destination = 'operating' AND destination_approval_id IS NOT NULL AND stripe_account_id IS NOT NULL AND trust_state IS NULL)
    )
);
CREATE INDEX matter_invoice_project ON matter_invoice (project_id, issued_on, id);
CREATE INDEX matter_invoice_external ON matter_invoice (external_id) WHERE external_id IS NOT NULL;

CREATE TABLE matter_invoice_payment (
    id UUID PRIMARY KEY,
    invoice_id UUID NOT NULL REFERENCES matter_invoice(id),
    amount_cents BIGINT NOT NULL CHECK (amount_cents > 0),
    received_on DATE NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('stripe', 'in_person', 'check', 'wire', 'other', 'xero')),
    external_id TEXT CHECK (external_id IS NULL OR length(btrim(external_id)) BETWEEN 1 AND 200),
    payer TEXT CHECK (payer IS NULL OR length(btrim(payer)) BETWEEN 1 AND 300),
    memo TEXT CHECK (memo IS NULL OR length(btrim(memo)) BETWEEN 1 AND 2000),
    client_visible BOOLEAN NOT NULL DEFAULT false,
    recorded_by UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (invoice_id, source, external_id)
);
CREATE INDEX matter_invoice_payment_invoice ON matter_invoice_payment (invoice_id, received_on, id);

-- Payments are evidence: corrected by a new row, never edited or deleted.
CREATE FUNCTION protect_matter_invoice_payment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'A Matter Invoice payment is retained; record a correction instead';
END;
$$;
CREATE TRIGGER matter_invoice_payment_retained BEFORE UPDATE OR DELETE ON matter_invoice_payment
    FOR EACH ROW EXECUTE FUNCTION protect_matter_invoice_payment();

-- Work, costs, batches and trust lines name their invoice by ledger row.
ALTER TABLE time_entry ADD COLUMN matter_invoice_id UUID REFERENCES matter_invoice(id);
ALTER TABLE project_costs ADD COLUMN matter_invoice_id UUID REFERENCES matter_invoice(id);
ALTER TABLE matter_billing_batch ADD COLUMN matter_invoice_id UUID UNIQUE REFERENCES matter_invoice(id);
ALTER TABLE iolta_allocation ADD COLUMN matter_invoice_id UUID REFERENCES matter_invoice(id);

-- Carry every Xero invoice into the ledger as history, then relink. Clients
-- saw these on the matter, so they stay client-visible. A Xero id work was
-- marked under without a mirrored invoice keeps a ledger row too, so no link
-- is lost. The audit triggers refuse unattributed updates; this relink
-- changes no billing state, so they are off only for it.
DO $$
BEGIN
INSERT INTO matter_invoice (id, project_id, source, external_id, reference, status, amount_cents, currency, issued_on, due_on, client_visible, inserted_at, updated_at)
SELECT gen_random_uuid(), x.project_id, 'xero', x.xero_invoice_id, x.reference,
    CASE upper(x.status)
        WHEN 'DRAFT' THEN 'draft' WHEN 'SUBMITTED' THEN 'draft'
        WHEN 'PAID' THEN 'paid' WHEN 'VOIDED' THEN 'void' WHEN 'DELETED' THEN 'void'
        ELSE 'sent' END,
    x.amount_cents, x.currency, (x.issued_at AT TIME ZONE 'UTC')::date,
    (x.due_at AT TIME ZONE 'UTC')::date, true, x.inserted_at, x.updated_at
FROM xero_invoice x JOIN project p ON p.id = x.project_id;

INSERT INTO matter_invoice_payment (id, invoice_id, amount_cents, received_on, source, external_id, memo, client_visible, inserted_at)
SELECT gen_random_uuid(), m.id, x.amount_paid_cents, (x.updated_at AT TIME ZONE 'UTC')::date, 'xero',
    x.xero_invoice_id || ':paid', 'Paid to date when Xero invoicing was retired', true, x.updated_at
FROM xero_invoice x JOIN matter_invoice m ON m.source = 'xero' AND m.external_id = x.xero_invoice_id
    AND m.project_id = x.project_id
WHERE x.amount_paid_cents > 0;

INSERT INTO matter_invoice (id, project_id, source, external_id, reference, status, amount_cents, currency, memo)
SELECT gen_random_uuid(), marked.project_id, 'xero', marked.xero_invoice_id,
    'Xero invoice ' || marked.xero_invoice_id, 'sent', 0, 'USD',
    'Recorded when Xero invoicing was retired; the Xero invoice was never mirrored'
FROM (
    SELECT project_id, xero_invoice_id FROM time_entry WHERE xero_invoice_id IS NOT NULL
    UNION SELECT project_id, xero_invoice_id FROM project_costs WHERE xero_invoice_id IS NOT NULL
    UNION SELECT project_id, xero_invoice_id FROM matter_billing_batch WHERE xero_invoice_id IS NOT NULL
    UNION SELECT project_id, xero_invoice_id FROM iolta_allocation WHERE xero_invoice_id IS NOT NULL
) marked
WHERE NOT EXISTS (SELECT 1 FROM matter_invoice m WHERE m.source = 'xero'
    AND m.external_id = marked.xero_invoice_id AND m.project_id = marked.project_id);

ALTER TABLE time_entry DISABLE TRIGGER time_entry_audit_update;
ALTER TABLE time_entry DISABLE TRIGGER time_entry_stage_firm;
UPDATE time_entry t SET matter_invoice_id = m.id FROM matter_invoice m
WHERE t.xero_invoice_id IS NOT NULL AND m.source = 'xero' AND m.external_id = t.xero_invoice_id
    AND m.project_id = t.project_id;
ALTER TABLE time_entry ENABLE TRIGGER time_entry_audit_update;
ALTER TABLE time_entry ENABLE TRIGGER time_entry_stage_firm;

ALTER TABLE project_costs DISABLE TRIGGER project_costs_audit_update;
UPDATE project_costs c SET matter_invoice_id = m.id FROM matter_invoice m
WHERE c.xero_invoice_id IS NOT NULL AND m.source = 'xero' AND m.external_id = c.xero_invoice_id
    AND m.project_id = c.project_id;
ALTER TABLE project_costs ENABLE TRIGGER project_costs_audit_update;

UPDATE matter_billing_batch b SET matter_invoice_id = m.id FROM matter_invoice m
WHERE b.xero_invoice_id IS NOT NULL AND m.source = 'xero' AND m.external_id = b.xero_invoice_id
    AND m.project_id = b.project_id;

UPDATE iolta_allocation a SET matter_invoice_id = m.id FROM matter_invoice m
WHERE m.source = 'xero' AND m.external_id = a.xero_invoice_id AND m.project_id = a.project_id;
END $$;

ALTER TABLE time_entry DROP CONSTRAINT time_entry_status_evidence;
ALTER TABLE time_entry DROP COLUMN xero_invoice_id;
ALTER TABLE time_entry ADD CONSTRAINT time_entry_status_evidence CHECK ((billing_status IN ('unbilled', 'reserved') AND matter_invoice_id IS NULL AND written_off_reason IS NULL) OR (billing_status = 'invoiced' AND matter_invoice_id IS NOT NULL AND written_off_reason IS NULL) OR (billing_status = 'written_off' AND matter_invoice_id IS NULL AND written_off_reason IS NOT NULL));
CREATE INDEX time_entry_matter_invoice ON time_entry (matter_invoice_id) WHERE matter_invoice_id IS NOT NULL;

ALTER TABLE project_costs DROP CONSTRAINT project_costs_check;
ALTER TABLE project_costs DROP COLUMN xero_invoice_id;
ALTER TABLE project_costs ADD CONSTRAINT project_costs_invoiced_evidence CHECK ((billing_status = 'invoiced') = (matter_invoice_id IS NOT NULL));

-- A Matter Billing Batch collects through Stripe; a completed Xero batch
-- keeps its invoice as a ledger row.
ALTER TABLE matter_billing_batch DROP CONSTRAINT matter_billing_batch_issuer;
ALTER TABLE matter_billing_batch DROP COLUMN xero_invoice_id;
ALTER TABLE matter_billing_batch DROP COLUMN xero_contact_id;
ALTER TABLE matter_billing_batch ADD CONSTRAINT matter_billing_batch_issuer CHECK ((stripe_account_id IS NOT NULL) OR (NOT stripe_iolta AND stripe_invoice_id IS NULL));

-- A trust withdrawal line settles a ledger invoice.
DROP INDEX iolta_allocation_withdrawal_invoice_key;
ALTER TABLE iolta_allocation DROP COLUMN xero_invoice_id;
ALTER TABLE iolta_allocation ALTER COLUMN matter_invoice_id SET NOT NULL;
CREATE UNIQUE INDEX iolta_allocation_withdrawal_invoice_key ON iolta_allocation (withdrawal_id, matter_invoice_id);

DROP TABLE xero_invoice;
