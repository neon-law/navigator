ALTER TABLE firm ADD COLUMN workspace_provider TEXT NOT NULL DEFAULT 'google_workspace' CHECK (workspace_provider IN ('google_workspace', 'microsoft_365'));
