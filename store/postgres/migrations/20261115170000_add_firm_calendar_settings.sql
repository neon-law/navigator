CREATE TABLE firm_calendar_settings (
    firm_id uuid PRIMARY KEY REFERENCES firm(id) ON DELETE CASCADE,
    google_workspace_owner text
);
