-- The exhibit metadata the matter page's evidence index reads (LAW-185):
-- `{ dated_at, source, confidence, supports }` as a JSON object. NULL for
-- rows written before exhibits carried provenance; the shape is
-- `store::documents::Evidence`, validated before the write.
ALTER TABLE asset ADD COLUMN evidence JSONB;
