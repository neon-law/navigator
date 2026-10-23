-- A Client's Purchase of an Offering. Born in PostgreSQL with no Surreal
-- source. The Offering, its price, and its cadence are copied from the
-- services catalog at the moment of purchase, so a later price change never
-- rewrites what a Client was invoiced. Xero raises and collects the invoice;
-- this row records which invoice answers for the Purchase and turns `active`
-- once Xero reports it paid.
CREATE TABLE purchase (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL,
    email TEXT NOT NULL CHECK (email <> ''),
    offering TEXT NOT NULL CHECK (offering ~ '^[a-z0-9-]{1,64}$'),
    offering_name TEXT NOT NULL CHECK (offering_name <> ''),
    cadence TEXT NOT NULL CHECK (cadence IN ('once', 'month', 'year')),
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    account_code TEXT NOT NULL CHECK (account_code ~ '^[A-Za-z0-9]{1,10}$'),
    state TEXT NOT NULL CHECK (state IN ('complimentary', 'invoicing', 'invoiced', 'active')),
    xero_contact_id TEXT,
    xero_invoice_id TEXT UNIQUE,
    invoice_url TEXT,
    xero_repeating_invoice_id TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    activated_at TIMESTAMPTZ,
    CHECK ((state = 'complimentary') = (amount_cents = 0)),
    CHECK (state NOT IN ('invoiced', 'active') OR xero_invoice_id IS NOT NULL),
    CHECK ((state = 'active') = (activated_at IS NOT NULL))
);
-- At most one unpaid invoice per Client and Offering, so a double click or a
-- retry finds the open Purchase instead of raising a second invoice.
CREATE UNIQUE INDEX purchase_one_open ON purchase (person_id, offering) WHERE state IN ('invoicing', 'invoiced');
CREATE INDEX purchase_person ON purchase (person_id, inserted_at);
CREATE INDEX purchase_invoiced ON purchase (state, updated_at) WHERE state = 'invoiced';
