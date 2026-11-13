-- The signing date a pleading's date line prints, `Dated this <ordinal> day
-- of <Month>, <year>` (LAW-242). Sourced only from the matter manifest's
-- `signed_on:`; NULL while the lawyer has not set a signing date.
ALTER TABLE matter_caption ADD COLUMN signed_on TEXT;
