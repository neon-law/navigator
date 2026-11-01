-- PostgreSQL-authoritative Project Folder identities. Remote permission
-- reconciliation is separate; a binding never grants Navigator access.
CREATE TABLE project_folder (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    slot TEXT NOT NULL CHECK (slot IN ( 'root', 'client_submissions', 'Client Production', 'Correspondence (Served)', 'Discovery (Served)', 'Docket Filings', 'Court Rules', 'Engagement', 'Work Product', 'Work Product/Client Calls (Work Product)', 'Work Product/Correspondence (Work Product)', 'Work Product/Discovery (Work Product)', 'Work Product/Motions (Work Product)', 'Work Product/Pleadings (Work Product)', 'Work Product/Research (Work Product)', 'Work Product/Stipulations (Work Product)' )),
    drive_folder_id TEXT NOT NULL UNIQUE CHECK (length(drive_folder_id) BETWEEN 1 AND 255),
    parent_id UUID,
    parent_drive_folder_id TEXT CHECK (parent_drive_folder_id IS NULL OR length(parent_drive_folder_id) BETWEEN 1 AND 255),
    inserted_at TIMESTAMPTZ NOT NULL,
    verified_at TIMESTAMPTZ,
    UNIQUE (project_id, slot),
    UNIQUE (project_id, id),
    FOREIGN KEY (project_id, parent_id) REFERENCES project_folder(project_id, id),
    CHECK (parent_id IS NULL OR parent_id <> id),
    CHECK ((slot IN ('root', 'client_submissions') AND parent_id IS NULL) OR (slot NOT IN ('root', 'client_submissions') AND parent_id IS NOT NULL))
);
