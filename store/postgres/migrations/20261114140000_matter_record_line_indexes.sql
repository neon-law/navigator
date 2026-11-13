-- Matter authority enforces in PostgreSQL the one-row-per-line rules the
-- SurrealDB schema enforces, under the same index names.
CREATE UNIQUE INDEX estate_asset_line ON estate_asset (project_id, category, description);
CREATE UNIQUE INDEX fiduciary_role_order ON fiduciary_role (project_id, role, successor_order);
CREATE UNIQUE INDEX cap_table_holding_line ON cap_table_holding (project_id, holder_id, share_class);
