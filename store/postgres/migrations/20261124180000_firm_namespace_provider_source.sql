-- Preserve the independently verified provider name when a Firm and its existing
-- Forgejo organization start with different names. Earlier receipts used old_slug for both.
ALTER TABLE firm_namespace_move ADD COLUMN old_organization TEXT CHECK (old_organization ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(old_organization) <= 40 AND old_organization <> new_slug);

CREATE OR REPLACE FUNCTION project_namespace_current() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.firm_namespace_move m JOIN public.firm f ON f.id = m.firm_id
               WHERE m.completed_at IS NOT NULL
                 AND (starts_with(lower(NEW.repository_url), lower(m.forge_origin || '/' || m.old_slug || '/'))
                      OR starts_with(lower(NEW.repository_url), lower(m.forge_origin || '/' || COALESCE(m.old_organization,m.old_slug) || '/')))
                 AND (NEW.firm_id IS DISTINCT FROM m.firm_id OR NOT starts_with(lower(NEW.repository_url), lower(m.forge_origin || '/' || f.slug || '/')))) THEN
        RAISE EXCEPTION 'Project repository uses a retired Firm namespace' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
