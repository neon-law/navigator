-- A pushed preview may reference the real run it opened. Historical
-- drafts retain NULL and remain readable without inventing a Notation.
ALTER TABLE notation_draft ADD COLUMN notation_id UUID REFERENCES notation (id);
