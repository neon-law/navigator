CREATE INDEX project_deadlines_open_due_date
    ON project_deadlines (
        project_id,
        (COALESCE(
            (due_on->>'date')::date,
            DATE '1970-01-01' + floor(extract(epoch FROM due_at) / 86400)::integer
        )),
        due_at,
        id
    )
    WHERE status = 'open';
