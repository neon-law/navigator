-- A deployment operator records the existing organization and its bound
-- repositories before calling Forgejo. The receipt survives an interrupted call.
CREATE TABLE firm_namespace_move (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id),
    forge_origin TEXT NOT NULL,
    forgejo_id BIGINT NOT NULL CHECK (forgejo_id > 0),
    old_slug TEXT NOT NULL,
    new_slug TEXT NOT NULL,
    repositories JSONB NOT NULL CHECK (jsonb_typeof(repositories) = 'object'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK (old_slug <> new_slug)
);
CREATE UNIQUE INDEX firm_namespace_move_pending ON firm_namespace_move (firm_id) WHERE completed_at IS NULL;

-- Default privileges may have granted runtime writes to newly created tables.
DO $$
BEGIN
    IF to_regrole('navigator_app') IS NOT NULL THEN
        REVOKE ALL ON firm_namespace_move FROM navigator_app;
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION firm_slug_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.slug IS NOT DISTINCT FROM OLD.slug THEN
        RETURN NEW;
    END IF;
    -- Only the table owner acting on one recorded intent can change the slug.
    -- A transaction-local setting alone confers no authority.
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        IF EXISTS (SELECT 1 FROM public.firm_namespace_move
                   WHERE id::text = current_setting('navigator.firm_namespace_move', true)
                     AND firm_id = OLD.id AND old_slug = OLD.slug AND new_slug = NEW.slug AND completed_at IS NULL) THEN
            RETURN NEW;
        END IF;
    END IF;
    RAISE EXCEPTION 'firm.slug is immutable' USING ERRCODE = 'check_violation';
END;
$$;

-- Old checkout coordinates can arrive after maintenance ends. Refuse an old
-- path for every tenant, including writes replayed by an importer. A completed
-- reverse move makes its current slug admissible again for its own Firm.
CREATE FUNCTION project_namespace_current() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.firm_namespace_move m JOIN public.firm f ON f.id = m.firm_id
               WHERE m.completed_at IS NOT NULL
                 AND starts_with(lower(NEW.repository_url), lower(m.forge_origin || '/' || m.old_slug || '/'))
                 AND (NEW.firm_id IS DISTINCT FROM m.firm_id OR f.slug <> m.old_slug)) THEN
        RAISE EXCEPTION 'Project repository uses a retired Firm namespace' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_namespace_current BEFORE INSERT OR UPDATE OF repository_url, firm_id ON project
    FOR EACH ROW WHEN (NEW.repository_url IS NOT NULL) EXECUTE FUNCTION project_namespace_current();
