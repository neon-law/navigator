CREATE TABLE astrocartography_reference_geometry (
    specimen_sha256 TEXT NOT NULL CHECK (specimen_sha256 ~ '^[0-9a-f]{64}$'),
    instant_utc TIMESTAMPTZ NOT NULL,
    body TEXT NOT NULL,
    angle TEXT NOT NULL,
    segments geometry(MultiLineString,4326) NOT NULL,
    PRIMARY KEY (specimen_sha256, body, angle),
    CHECK (NOT ST_IsEmpty(segments) AND ST_IsValid(segments))
);

CREATE INDEX astrocartography_reference_geometry_segments
    ON astrocartography_reference_geometry USING gist (segments);

-- Fixed Sun MC from webapp/src/assets/world-specimen-lines.json. Its 181
-- ordered vertices lie on one meridian, sampled at integer latitudes.
DO $$
BEGIN
    INSERT INTO astrocartography_reference_geometry (specimen_sha256, instant_utc, body, angle, segments)
    SELECT 'ab699abf3265459b1a9a25d46ab20211e4adf35610898fc7f165e9a28e6f496d',
           TIMESTAMPTZ '2000-01-01 12:00:00+00', 'sun', 'mc',
           ST_Multi(ST_SetSRID(ST_MakeLine(ST_MakePoint(0.8198348255188534, latitude) ORDER BY latitude), 4326))
    FROM generate_series(-90, 90) AS latitude;

    -- The migrator owns this immutable reference.
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'navigator_app') THEN
        GRANT SELECT ON astrocartography_reference_geometry TO navigator_app;
        REVOKE INSERT, UPDATE, DELETE ON astrocartography_reference_geometry FROM navigator_app;
    END IF;
END
$$;
