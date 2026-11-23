-- Keep the Project as a composite so the directory reads the current Project row type.
CREATE VIEW project_list WITH (security_invoker = true) AS
SELECT membership.person_id, membership.participation, membership.is_client_dri, p AS project
FROM person_project_role AS membership
JOIN project AS p ON p.id = membership.project_id;
