-- Manual entries have an owning Firm and a view scope. Other audit writers
-- retain their existing contract. The original Firm id survives Firm deletion.
ALTER TABLE relationship_log ADD COLUMN firm_id UUID;
ALTER TABLE relationship_log ADD COLUMN graph_scope TEXT;
ALTER TABLE relationship_log ADD CONSTRAINT relationship_log_firm_scope CHECK ((graph_scope IS NULL) = (firm_id IS NULL));
ALTER TABLE relationship_log ADD CONSTRAINT relationship_log_graph_scope CHECK (graph_scope IS NULL OR (actor_person_id IS NOT NULL AND subject_type IN ('person', 'entity') AND ((graph_scope = 'project' AND project_id IS NOT NULL) OR (graph_scope IN ('attorney', 'firm') AND project_id IS NULL))));

CREATE INDEX relationship_log_graph_firm
    ON relationship_log (firm_id, inserted_at DESC, id DESC)
    WHERE graph_scope IN ('project', 'firm');
CREATE INDEX relationship_log_graph_attorney
    ON relationship_log (actor_person_id, inserted_at DESC, id DESC)
    WHERE graph_scope IN ('project', 'attorney');
CREATE INDEX relationship_log_graph_project
    ON relationship_log (project_id, inserted_at DESC, id DESC)
    WHERE graph_scope = 'project';
