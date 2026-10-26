ALTER TABLE project_deadlines ADD COLUMN calendar_sequence BIGINT CHECK (calendar_sequence IS NULL OR calendar_sequence BETWEEN 0 AND 2147483647);
