-- Durable application-owned request identity for authenticated DBOS ingress.
-- The engine's global workflow ID is reserved for one typed request forever;
-- retries may reuse that request, while a different payload is rejected.
CREATE TABLE dbos_workflow_request (
    workflow_id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
