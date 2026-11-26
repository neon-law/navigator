-- One dedicated booking availability calendar per attorney.
ALTER TABLE person_calendar DROP CONSTRAINT person_calendar_kind_check;
ALTER TABLE person_calendar ADD CONSTRAINT person_calendar_kind_check CHECK (kind IN ('primary', 'freebusy', 'booking'));
CREATE UNIQUE INDEX person_calendar_one_booking ON person_calendar (person_id)
    WHERE kind = 'booking';
CREATE UNIQUE INDEX person_calendar_booking_owner ON person_calendar (calendar_id)
    WHERE kind = 'booking';
