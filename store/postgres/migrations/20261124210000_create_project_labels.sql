CREATE TABLE project_label (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (length(key) <= 64 AND key ~ '^[a-z0-9]+(-[a-z0-9]+)*$'),
    name TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 80),
    category TEXT NOT NULL CHECK (length(btrim(category)) BETWEEN 1 AND 80),
    retired_at TIMESTAMPTZ,
    UNIQUE (firm_id, key)
);

CREATE TABLE project_label_assignment (
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    label_id UUID NOT NULL REFERENCES project_label(id) ON DELETE CASCADE,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, label_id)
);

CREATE INDEX project_label_assignment_label ON project_label_assignment (label_id, project_id);
