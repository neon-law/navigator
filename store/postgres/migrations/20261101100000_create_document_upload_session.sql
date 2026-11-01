-- Upload Sessions retain metadata and the acknowledged offset; chunk bytes stay in object storage.
CREATE TABLE document_upload_session (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    filename text NOT NULL,
    kind text NOT NULL,
    content_type text NOT NULL,
    visibility text NOT NULL CHECK (visibility IN ('internal', 'client')),
    description text,
    slug text,
    transcript_quality text,
    metadata jsonb,
    derived_from jsonb,
    signing jsonb,
    notarization jsonb,
    declared_size bigint NOT NULL CHECK (declared_size > 0 AND declared_size <= 1073741824),
    declared_sha256 text NOT NULL,
    chunk_size bigint NOT NULL CHECK (chunk_size > 0),
    received_bytes bigint NOT NULL DEFAULT 0 CHECK (received_bytes >= 0 AND received_bytes <= declared_size),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX document_upload_session_resume
    ON document_upload_session (project_id, filename, declared_sha256, declared_size);
CREATE INDEX document_upload_session_expiry ON document_upload_session (expires_at);
