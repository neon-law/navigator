-- The litigation tables hold the application's authority for these rows once
-- NAVIGATOR_POSTGRES_HOST is set, so the unique semantics SurrealDB's schema
-- enforces on the same tables must hold here too: one docket entry per
-- (case, entry number), one discovery set per (case, device, set number),
-- and one discovery item per (request, item number).
CREATE UNIQUE INDEX case_docket_entry_case_number
    ON case_docket_entry (case_id, entry_number);
CREATE UNIQUE INDEX discovery_request_case_device_set
    ON discovery_request (case_id, device, set_number);
CREATE UNIQUE INDEX discovery_item_request_number
    ON discovery_item (discovery_request_id, item_number);
