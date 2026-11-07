\set ON_ERROR_STOP on
CREATE EXTENSION postgis;
SELECT postgis_full_version();

CREATE TABLE smoke_spatial (
    id integer PRIMARY KEY,
    location geometry(Point, 4326) NOT NULL,
    trace geometry(MultiLineString, 4326) NOT NULL
);
INSERT INTO smoke_spatial VALUES
    (1, ST_SetSRID(ST_MakePoint(179, 10), 4326),
        ST_GeomFromText('MULTILINESTRING((179 10,180 11),(-180 11,-179 12))', 4326)),
    (2, ST_SetSRID(ST_MakePoint(0, 0), 4326),
        ST_GeomFromText('MULTILINESTRING((0 0,1 1))', 4326));
CREATE INDEX smoke_spatial_location_gist ON smoke_spatial USING gist (location);
SET enable_seqscan = off;
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT FROM smoke_spatial
        WHERE id = 1 AND ST_IsValid(trace) AND ST_NumGeometries(trace) = 2
          AND ST_Equals(trace, ST_SetSRID(ST_GeomFromGeoJSON(ST_AsGeoJSON(trace)), 4326))
          AND ST_Equals(location, ST_SetSRID(ST_GeomFromGeoJSON(ST_AsGeoJSON(location)), 4326))
          AND ST_X(ST_EndPoint(ST_GeometryN(trace, 1))) = 180
          AND ST_X(ST_StartPoint(ST_GeometryN(trace, 2))) = -180
    ) THEN RAISE EXCEPTION 'WGS84 geometry round trip'; END IF;
    IF (SELECT array_agg(id) FROM smoke_spatial
        WHERE location && ST_MakeEnvelope(178, 9, 180, 12, 4326))
        IS DISTINCT FROM ARRAY[1] THEN RAISE EXCEPTION 'spatial bounding box'; END IF;
END $$;
