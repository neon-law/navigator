-- Every workflow and package pins an existing, retained Template version.
-- An orphan stops this transactional migration for source reconciliation.
ALTER TABLE notation ADD CONSTRAINT notation_template_version FOREIGN KEY (template_id) REFERENCES template(id);
ALTER TABLE notation_event ADD CONSTRAINT notation_event_template_version FOREIGN KEY (template_version_id) REFERENCES template(id);
ALTER TABLE notation_package_template ADD CONSTRAINT notation_package_template_version FOREIGN KEY (template_id) REFERENCES template(id);
