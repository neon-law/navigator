-- Fixed Sun IC from webapp/src/assets/world-specimen-lines.json.
-- Subtract 360 from the source longitude to store signed WGS84 coordinates.
DO $$
BEGIN
    INSERT INTO astrocartography_reference_geometry (specimen_sha256, instant_utc, body, angle, segments)
    SELECT 'ab699abf3265459b1a9a25d46ab20211e4adf35610898fc7f165e9a28e6f496d',
           TIMESTAMPTZ '2000-01-01 12:00:00+00', 'sun', 'ic',
           ST_Multi(ST_SetSRID(ST_MakeLine(ST_MakePoint(-179.18016517448115, latitude) ORDER BY latitude), 4326))
    FROM generate_series(-90, 90) AS latitude;
END
$$;
