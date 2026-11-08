-- The Project's client DRI Person — the client a CLIENT_DRI special question
-- and a `project.client_dri` Clause binding resolve to. NULL until recorded;
-- the write seam (`store::projects::set_client_dri_person_id`) validates the
-- value fails-closed against the matter's own participation rows.
ALTER TABLE project ADD COLUMN client_dri_person_id UUID REFERENCES person(id);
