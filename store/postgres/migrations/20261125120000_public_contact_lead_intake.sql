ALTER TABLE lead_intake ADD COLUMN contact_name TEXT;

ALTER TABLE lead_intake
    ALTER COLUMN school_district DROP NOT NULL,
    ALTER COLUMN relationship DROP NOT NULL,
    ADD CONSTRAINT lead_intake_answer_shape CHECK (
        (contact_name IS NULL AND school_district IS NOT NULL AND relationship IS NOT NULL)
        OR (contact_name IS NOT NULL AND school_district IS NULL AND relationship IS NULL AND next_date IS NULL)
    );
