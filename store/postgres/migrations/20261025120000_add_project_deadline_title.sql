-- A Deadline's short title: what is due. NULL for rows written before titles existed.
ALTER TABLE project_deadlines ADD COLUMN title TEXT CHECK (title IS NULL OR length(btrim(title)) BETWEEN 1 AND 200);
