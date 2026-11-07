-- The matter folder a document is filed in (LAW-241): one of the Firm's
-- matter folders (`store::document_pointers::MatterFolder`), as the
-- pointer's `folder:` key spells it, persisted so the live matter files the
-- document where the pointer says. NULL for rows written before the field
-- existed and for rows whose placement is the kind's default; the write
-- seam (`store::assets::set_folder`) validates the value fails-closed.
ALTER TABLE asset ADD COLUMN folder TEXT;
