-- Canonical writers serialize with reset through the Notation parent.
-- Imported historical orphans remain representable; physical reference
-- validation follows the scoped import's reference proof.
CREATE UNIQUE INDEX notation_document_notation ON notation_document (notation_id);
CREATE INDEX notation_document_version_document
    ON notation_document_version (document_id, inserted_at, id);
