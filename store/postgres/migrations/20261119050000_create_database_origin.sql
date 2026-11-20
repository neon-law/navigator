-- Initial authority of an actually newly created application database.
-- Captured-source enrollment remains on its existing independently verified ledger.
CREATE TABLE database_origin (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    origin TEXT NOT NULL CHECK (origin = 'fresh_native'),
    creation_operation UUID NOT NULL UNIQUE,
    database_name TEXT NOT NULL,
    database_oid BIGINT NOT NULL CHECK (database_oid > 0),
    tier_id UUID NOT NULL,
    namespace_uid UUID NOT NULL,
    cluster_name TEXT NOT NULL CHECK (length(cluster_name) BETWEEN 1 AND 63),
    schema_contract JSONB NOT NULL CHECK (jsonb_typeof(schema_contract) = 'array'),
    authority_contract JSONB NOT NULL CHECK (jsonb_typeof(authority_contract) = 'array'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE FUNCTION protect_database_origin() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Database Origin is immutable';
END;
$$;
CREATE TRIGGER database_origin_immutable BEFORE UPDATE OR DELETE ON database_origin
    FOR EACH ROW EXECUTE FUNCTION protect_database_origin();

-- A durable intent binds a real schema upgrade before SQLx commits its DDL.
-- Only completion may change; interruption never licenses an unknown schema.
CREATE TABLE database_origin_upgrade (
    upgrade_operation UUID PRIMARY KEY,
    creation_operation UUID NOT NULL REFERENCES database_origin(creation_operation),
    predecessor_operation UUID NOT NULL UNIQUE,
    from_schema_contract JSONB NOT NULL CHECK (jsonb_typeof(from_schema_contract) = 'array'),
    to_schema_contract JSONB NOT NULL CHECK (jsonb_typeof(to_schema_contract) = 'array'),
    authority_contract JSONB NOT NULL CHECK (jsonb_typeof(authority_contract) = 'array'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK (upgrade_operation <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (predecessor_operation <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (upgrade_operation <> creation_operation),
    CHECK (upgrade_operation <> predecessor_operation)
);
CREATE FUNCTION protect_database_origin_upgrade() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL AND
       (to_jsonb(OLD) - 'completed_at') = (to_jsonb(NEW) - 'completed_at') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'Database Origin upgrade intent is immutable';
END;
$$;
CREATE TRIGGER database_origin_upgrade_immutable BEFORE UPDATE OR DELETE ON database_origin_upgrade
    FOR EACH ROW EXECUTE FUNCTION protect_database_origin_upgrade();
REVOKE ALL ON database_origin_upgrade FROM PUBLIC;

-- The runtime cannot SELECT the migration ledger. This fixed readonly attestation
-- exposes only schema versions/checksums, with no caller-selected SQL or relation.
CREATE FUNCTION database_origin_schema_contract() RETURNS JSONB
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    RETURN (SELECT CASE WHEN bool_and(success)
        THEN jsonb_agg(jsonb_build_array(version, encode(checksum, 'hex')) ORDER BY version)
        ELSE NULL END
        FROM public._sqlx_migrations);
END;
$$;
REVOKE ALL ON FUNCTION database_origin_schema_contract() FROM PUBLIC;
REVOKE ALL ON database_origin FROM PUBLIC;

-- One policy is used after the operator's general runtime grants and by the
-- isolated least-privilege test. It confers no rights on its own caller.
CREATE FUNCTION protect_database_origin_runtime(runtime_role REGROLE) RETURNS VOID
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE
    role_name TEXT;
BEGIN
    SELECT rolname INTO STRICT role_name FROM pg_catalog.pg_roles WHERE oid = runtime_role;
    EXECUTE format('REVOKE ALL ON TABLE public.database_origin, public.database_origin_upgrade FROM %I', role_name);
    EXECUTE format('GRANT SELECT ON TABLE public.database_origin, public.database_origin_upgrade TO %I', role_name);
    EXECUTE format('REVOKE ALL ON TABLE public.durable_object_handover, public.durable_object_ownership FROM %I', role_name);
    EXECUTE format('GRANT SELECT ON TABLE public.durable_object_handover, public.durable_object_ownership TO %I', role_name);
    EXECUTE format('GRANT EXECUTE ON FUNCTION public.database_origin_schema_contract() TO %I', role_name);
    EXECUTE format('REVOKE ALL ON FUNCTION public.protect_database_origin_runtime(regrole) FROM %I', role_name);
END;
$$;
REVOKE ALL ON FUNCTION protect_database_origin_runtime(REGROLE) FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'navigator_app') THEN
        PERFORM public.protect_database_origin_runtime('navigator_app'::regrole);
    END IF;
END;
$$;
