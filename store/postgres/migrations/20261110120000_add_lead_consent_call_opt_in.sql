-- The callback choice a Lead makes beside the SMS one
-- (docs/glossary/lead-consent.md): the exact wording shown and whether the
-- box was ticked. Rows captured before the choice existed carry no wording
-- and no consent to a call.
ALTER TABLE lead_consent ADD COLUMN call_consent_version TEXT;
ALTER TABLE lead_consent ADD COLUMN call_opt_in BOOLEAN NOT NULL DEFAULT false;
