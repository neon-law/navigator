-- Purchases collect through Stripe only (LAW-507). A one-time Purchase is a
-- sent Stripe invoice on the Firm's Operating account; a recurring one stays
-- hosted Stripe Checkout. Xero no longer raises, reads, or cancels a
-- Purchase invoice.
--
-- History: a Purchase Xero already invoiced keeps that invoice id, and a
-- legacy Xero subscription keeps its repeating-invoice id, both read-only in
-- `legacy_xero_*`, so the Firm can reconcile them in Xero. The Xero contact
-- id is dropped. A reservation Xero never answered, or an unpaid Xero
-- invoice Navigator can no longer check, is expired: it stops blocking a new
-- Purchase, and any payment it received is reconciled by hand.
ALTER TABLE purchase DROP CONSTRAINT purchase_provider_receipt;
ALTER TABLE purchase DROP CONSTRAINT purchase_stripe_admission;
ALTER TABLE purchase DROP CONSTRAINT purchase_stripe_activation;
ALTER TABLE purchase ADD COLUMN legacy_xero_invoice_id TEXT;
ALTER TABLE purchase ADD COLUMN legacy_xero_repeating_invoice_id TEXT;
ALTER TABLE purchase ADD COLUMN stripe_invoice_id TEXT UNIQUE CHECK (stripe_invoice_id IS NULL OR stripe_invoice_id ~ '^in_[A-Za-z0-9]+$');

DO $$
BEGIN
    UPDATE purchase SET legacy_xero_invoice_id = xero_invoice_id,
        legacy_xero_repeating_invoice_id = xero_repeating_invoice_id;
    UPDATE purchase SET state = 'expired', updated_at = now()
        WHERE stripe_account_id IS NULL AND state IN ('invoicing', 'invoiced');
    -- A one-time Purchase never competes with a subscription; the index is
    -- recreated below with that filter.
    DROP INDEX purchase_one_stripe_subscription;
END $$;

ALTER TABLE purchase DROP COLUMN xero_invoice_id;
ALTER TABLE purchase DROP COLUMN xero_repeating_invoice_id;
ALTER TABLE purchase DROP COLUMN xero_contact_id;

-- Every unpaid, invoiced, or active Purchase is bound to Stripe, or is Xero history.
ALTER TABLE purchase ADD CONSTRAINT purchase_provider CHECK (state IN ('complimentary', 'expired') OR stripe_account_id IS NOT NULL OR legacy_xero_invoice_id IS NOT NULL);
-- An invoiced or active Purchase names exactly one provider receipt.
ALTER TABLE purchase ADD CONSTRAINT purchase_provider_receipt CHECK (state NOT IN ('invoiced', 'active') OR num_nonnulls(legacy_xero_invoice_id, stripe_checkout_session_id, stripe_invoice_id) = 1);
-- A one-time Stripe Purchase is an invoice with no renewal terms; a recurring one is hosted Checkout with accepted terms.
ALTER TABLE purchase ADD CONSTRAINT purchase_stripe_admission CHECK ((stripe_account_id IS NULL AND stripe_firm_id IS NULL AND stripe_invoice_id IS NULL) OR (stripe_account_id IS NOT NULL AND stripe_firm_id IS NOT NULL AND legacy_xero_invoice_id IS NULL AND ((cadence = 'once' AND renewal_terms IS NULL AND stripe_checkout_session_id IS NULL) OR (cadence <> 'once' AND renewal_terms IS NOT NULL AND btrim(renewal_terms) <> '' AND renewal_consented_at IS NOT NULL AND stripe_invoice_id IS NULL))));
-- Activation needs the paid invoice, or the paid subscription and its period.
ALTER TABLE purchase ADD CONSTRAINT purchase_stripe_activation CHECK (stripe_account_id IS NULL OR state <> 'active' OR (cadence = 'once' AND stripe_invoice_id IS NOT NULL) OR (cadence <> 'once' AND stripe_customer_id IS NOT NULL AND stripe_subscription_id IS NOT NULL AND stripe_period_end IS NOT NULL));

-- A Client cannot open monthly and yearly Stripe checkout concurrently.
CREATE UNIQUE INDEX purchase_one_stripe_subscription ON purchase (person_id)
    WHERE stripe_account_id IS NOT NULL AND cadence <> 'once' AND renewal_cancelled_at IS NULL AND state <> 'expired';
