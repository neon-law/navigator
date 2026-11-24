-- ------------------------------
-- TABLE: playbook
-- ------------------------------
DEFINE TABLE playbook SCHEMALESS;
-- TABLE DATA: playbook
CREATE playbook:⟨00000000-0000-4000-8000-000000000001⟩ CONTENT { name: 'Synthetic import' };
-- TABLE: draft
DEFINE TABLE draft AS SELECT * FROM template;
-- TABLE DATA: draft
CREATE draft:1 CONTENT { title: 'Derived row' };
