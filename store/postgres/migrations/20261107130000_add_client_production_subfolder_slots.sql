-- LAW-215 declares the two Client Production subfolders — Agreements and
-- Screenshots — in the MatterFolder vocabulary. The project_folder slot check
-- still pins the pre-nested list, so reconciling a slot beneath Client
-- Production fails its insert. Widen the check in place; the slots are plain
-- text rows, so no data migration rides along.
ALTER TABLE project_folder DROP CONSTRAINT project_folder_slot_check;

ALTER TABLE project_folder ADD CONSTRAINT project_folder_slot_check CHECK (slot IN ( 'root', 'client_submissions', 'Client Production', 'Client Production/Agreements', 'Client Production/Screenshots', 'Correspondence (Served)', 'Discovery (Served)', 'Docket Filings', 'Court Rules', 'Engagement', 'Work Product', 'Work Product/Client Calls (Work Product)', 'Work Product/Correspondence (Work Product)', 'Work Product/Discovery (Work Product)', 'Work Product/Motions (Work Product)', 'Work Product/Pleadings (Work Product)', 'Work Product/Research (Work Product)', 'Work Product/Stipulations (Work Product)' ));
