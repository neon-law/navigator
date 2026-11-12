-- The fixed Costs folder is provisioned beside the other top-level Matter Folders.
ALTER TABLE project_folder DROP CONSTRAINT project_folder_slot_check;

ALTER TABLE project_folder ADD CONSTRAINT project_folder_slot_check CHECK (slot IN ( 'root', 'client_submissions', 'Client Production', 'Client Production/Agreements', 'Client Production/Screenshots', 'Correspondence (Served)', 'Costs', 'Discovery (Served)', 'Docket Filings', 'Court Rules', 'Engagement', 'Work Product', 'Work Product/Client Calls (Work Product)', 'Work Product/Correspondence (Work Product)', 'Work Product/Discovery (Work Product)', 'Work Product/Motions (Work Product)', 'Work Product/Pleadings (Work Product)', 'Work Product/Research (Work Product)', 'Work Product/Stipulations (Work Product)' ));
