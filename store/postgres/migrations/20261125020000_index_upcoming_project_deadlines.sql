CREATE INDEX project_deadlines_open_due_date
    ON project_deadlines (
        project_id,
        (COALESCE(
            (due_on->>'date')::date,
            (due_at AT TIME ZONE 'UTC')::date
        )),
        due_at,
        id
    )
    WHERE status = 'open';
