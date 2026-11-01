-- Where a Project Cost's money came from: `mercury::<transaction-id>` for
-- Mercury-sourced costs. NULL for costs recorded before source references
-- were required; new costs are validated at the store boundary.
ALTER TABLE project_costs ADD COLUMN source_reference TEXT;
