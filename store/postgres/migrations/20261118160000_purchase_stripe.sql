-- Hosted subscription checkout retains the accepted terms and the account
-- whose customer and subscription identifiers the application may manage.
ALTER TABLE purchase
    ADD COLUMN stripe_firm_id UUID,
    ADD COLUMN stripe_account_id TEXT,
    ADD COLUMN stripe_checkout_session_id TEXT UNIQUE,
    ADD COLUMN stripe_customer_id TEXT,
    ADD COLUMN stripe_subscription_id TEXT UNIQUE,
    ADD COLUMN stripe_period_end TIMESTAMPTZ,
    ADD COLUMN stripe_cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN renewal_terms TEXT,
    ADD COLUMN renewal_consented_at TIMESTAMPTZ,
    ADD COLUMN renewal_cancelled_at TIMESTAMPTZ,
    ADD COLUMN access_ends_at TIMESTAMPTZ;

ALTER TABLE purchase DROP CONSTRAINT purchase_state_check;
ALTER TABLE purchase ADD CONSTRAINT purchase_state_check CHECK (
    state IN ('complimentary', 'invoicing', 'invoiced', 'active', 'expired')
);
ALTER TABLE purchase DROP CONSTRAINT purchase_check1;
ALTER TABLE purchase ADD CONSTRAINT purchase_provider_receipt CHECK (
    state NOT IN ('invoiced', 'active') OR
    ((xero_invoice_id IS NOT NULL) <> (stripe_checkout_session_id IS NOT NULL))
);
ALTER TABLE purchase ADD CONSTRAINT purchase_stripe_admission CHECK (
    (stripe_account_id IS NULL AND stripe_firm_id IS NULL) OR
    (stripe_account_id IS NOT NULL AND stripe_firm_id IS NOT NULL AND
     cadence <> 'once' AND renewal_terms IS NOT NULL AND btrim(renewal_terms) <> '' AND
     renewal_consented_at IS NOT NULL AND xero_invoice_id IS NULL)
);
ALTER TABLE purchase ADD CONSTRAINT purchase_stripe_activation CHECK (
    stripe_account_id IS NULL OR state <> 'active' OR
    (stripe_customer_id IS NOT NULL AND stripe_subscription_id IS NOT NULL AND stripe_period_end IS NOT NULL)
);

-- A Client cannot open monthly and yearly Stripe checkout concurrently.
CREATE UNIQUE INDEX purchase_one_stripe_subscription ON purchase (person_id)
    WHERE stripe_account_id IS NOT NULL AND renewal_cancelled_at IS NULL AND state <> 'expired';
