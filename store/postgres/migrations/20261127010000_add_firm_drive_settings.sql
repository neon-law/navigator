CREATE TABLE firm_drive_settings (
    firm_id uuid PRIMARY KEY REFERENCES firm(id) ON DELETE CASCADE,
    google_drive boolean NOT NULL
);

-- Firms already filing into Drive keep it; every other Firm starts with Drive off.
DO $$
BEGIN
INSERT INTO firm_drive_settings (firm_id, google_drive)
SELECT DISTINCT firm_id, true FROM project WHERE firm_id IS NOT NULL AND drive_folder_id IS NOT NULL;
END $$;
