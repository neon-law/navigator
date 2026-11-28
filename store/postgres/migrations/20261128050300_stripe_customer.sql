-- One Stripe Customer per billed Person in each of the Firm's Stripe
-- accounts (ENG-1342, LAW-571). A draft reuses it rather than creating a
-- Customer per invoice. Stripe ids are not secrets; keys never land here.
CREATE TABLE stripe_customer (
    firm_id UUID NOT NULL REFERENCES firm(id),
    stripe_account_id TEXT NOT NULL CHECK (stripe_account_id ~ '^acct_[A-Za-z0-9]+$'),
    person_id UUID NOT NULL REFERENCES person(id),
    customer_id TEXT NOT NULL CHECK (customer_id ~ '^cus_[A-Za-z0-9]+$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (stripe_account_id, person_id)
);
CREATE INDEX stripe_customer_firm ON stripe_customer (firm_id);

-- The Person a batch bills, so its draft finds that Person's Customer.
ALTER TABLE matter_billing_batch ADD COLUMN billed_person_id UUID REFERENCES person(id);
