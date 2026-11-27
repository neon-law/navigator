ALTER TABLE project_deadlines DROP CONSTRAINT project_deadlines_category_check;
ALTER TABLE project_deadlines ADD CONSTRAINT project_deadlines_category_check CHECK (category IN ('court', 'hearing', 'discovery', 'internal', 'tickler', 'trial_day', 'deposition', 'conference', 'consult', 'call'));
ALTER TABLE project_deadlines ADD COLUMN effort_minutes INTEGER NOT NULL DEFAULT 0 CHECK (effort_minutes >= 0);

DO $$
BEGIN
    UPDATE project_deadlines SET effort_minutes = CASE category
        WHEN 'court' THEN 360
        WHEN 'discovery' THEN 240
        WHEN 'internal' THEN 30
        WHEN 'tickler' THEN 15
        ELSE 0 END;
END $$;

ALTER TABLE firm_calendar_settings ADD COLUMN deadline_buffer_minutes INTEGER NOT NULL DEFAULT 15 CHECK (deadline_buffer_minutes BETWEEN 10 AND 1440);
ALTER TABLE firm_calendar_settings ADD COLUMN deadline_end_of_business TEXT NOT NULL DEFAULT '17:00:00' CHECK (deadline_end_of_business ~ '^([01][0-9]|2[0-3]):[0-5][0-9]:00$');
ALTER TABLE firm_calendar_settings ADD COLUMN deadline_effort_defaults JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(deadline_effort_defaults) = 'object');
