CREATE INDEX project_deadlines_open_due_at
    ON project_deadlines (project_id, due_at, id)
    WHERE status = 'open';
