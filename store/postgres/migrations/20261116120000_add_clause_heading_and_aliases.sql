-- Reserved-heading data on the shared Clause read model (ENG-1091 · LAW-142).
-- `rules/clauses/library.yaml` is the source of truth; these columns mirror
-- the optional `heading:` and `aliases:` keys a library version may declare.
ALTER TABLE clause ADD COLUMN heading TEXT;

ALTER TABLE clause ADD COLUMN aliases JSONB;
