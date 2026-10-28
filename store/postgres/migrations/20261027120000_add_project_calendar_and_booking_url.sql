-- A Project's shared Google Calendar id and its client booking link. NULL until recorded.
ALTER TABLE project ADD COLUMN calendar_id TEXT;
ALTER TABLE project ADD COLUMN booking_url TEXT;
