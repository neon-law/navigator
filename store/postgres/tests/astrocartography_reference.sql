-- The fixed Sun reference crosses the equator at its calculated longitude.
DO $$
DECLARE
    reference geometry;
BEGIN
    SELECT segments INTO STRICT reference
    FROM astrocartography_reference_geometry
    WHERE body = 'sun' AND angle = 'mc'
      AND instant_utc = TIMESTAMPTZ '2000-01-01 12:00:00+00';

    IF ST_SRID(reference) <> 4326
        OR ST_GeometryType(reference) <> 'ST_MultiLineString'
        OR NOT ST_IsValid(reference)
        OR ST_NumGeometries(reference) <> 1
        OR ST_NPoints(reference) <> 181
        OR ST_Y(ST_StartPoint(ST_GeometryN(reference, 1))) <> -90
        OR ST_Y(ST_EndPoint(ST_GeometryN(reference, 1))) <> 90
        OR NOT ST_Intersects(reference, ST_MakeEnvelope(0.8, -1, 0.9, 1, 4326))
        OR ST_Intersects(reference, ST_MakeEnvelope(-1, -1, 0, 1, 4326))
        OR NOT ST_OrderingEquals(reference, ST_SetSRID(ST_GeomFromGeoJSON(ST_AsGeoJSON(reference, 17)), 4326))
    THEN
        RAISE EXCEPTION 'fixed Sun meridian failed its WGS84 round trip';
    END IF;

    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        IF NOT has_table_privilege('navigator_app', 'astrocartography_reference_geometry', 'SELECT')
            OR has_table_privilege('navigator_app', 'astrocartography_reference_geometry', 'INSERT,UPDATE,DELETE')
        THEN
            RAISE EXCEPTION 'runtime must have read-only access to the fixed reference';
        END IF;
    END IF;
END
$$;

-- The lower meridian is west of the antimeridian in signed WGS84 coordinates.
DO $$
DECLARE
    reference geometry;
BEGIN
    SELECT lower_line.segments INTO STRICT reference
    FROM astrocartography_reference_geometry AS lower_line
    JOIN astrocartography_reference_geometry AS upper_line
      USING (specimen_sha256, instant_utc, body)
    WHERE lower_line.body = 'sun' AND lower_line.angle = 'ic' AND upper_line.angle = 'mc';

    IF ST_NumGeometries(reference) <> 1
        OR ST_NPoints(reference) <> 181
        OR ST_Y(ST_StartPoint(ST_GeometryN(reference, 1))) <> -90
        OR ST_Y(ST_EndPoint(ST_GeometryN(reference, 1))) <> 90
        OR NOT ST_Intersects(reference, ST_MakeEnvelope(-179.2, -1, -179.1, 1, 4326))
        OR ST_Intersects(reference, ST_MakeEnvelope(179.1, -1, 180, 1, 4326))
        OR NOT ST_OrderingEquals(reference, ST_SetSRID(ST_GeomFromGeoJSON(ST_AsGeoJSON(reference, 17)), 4326))
    THEN
        RAISE EXCEPTION 'fixed Sun lower meridian failed its signed WGS84 round trip';
    END IF;
END
$$;
