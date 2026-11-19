-- The e-signature provider a Firm sends its signature requests through. NULL means DocuSign, the
-- provider every Firm used before this column. A Brand reaches its provider through `brand.firm_id`.
-- `store::signatures::SignatureProvider` is the registry that validates the value.
ALTER TABLE firm ADD COLUMN esign_provider TEXT;
