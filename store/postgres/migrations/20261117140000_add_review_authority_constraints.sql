-- The review tables hold the application's authority for these rows once
-- NAVIGATOR_POSTGRES_HOST is set, so the unique semantics SurrealDB's schema
-- enforces on the same tables must hold here too: one review document per
-- (notation, kind) — the slot `store::review_documents::upsert_draft` races
-- through — and the index the sidebar's per-document comment read uses.
CREATE UNIQUE INDEX review_document_notation_kind
    ON review_document (notation_id, kind);
CREATE INDEX document_comment_review_document
    ON document_comment (review_document_id);
