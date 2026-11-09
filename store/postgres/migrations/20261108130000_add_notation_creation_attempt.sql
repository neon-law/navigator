CREATE TABLE notation_creation_attempt (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    template_code text NOT NULL,
    respondent_id uuid NOT NULL,
    failed_at timestamptz NOT NULL DEFAULT now(),
    notation_id uuid
);

CREATE INDEX notation_creation_attempt_project_idx
    ON notation_creation_attempt (project_id, template_code, respondent_id)
    WHERE notation_id IS NULL;
