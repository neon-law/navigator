-- A Firm's Portal Theme Brand: the key of one Brand the Firm owns, whose look
-- every `/app` page reached through the Firm's hosts wears. NULL keeps each
-- host's own Brand. `store::firms::update` checks that the Firm owns the
-- Brand, and the `/app` resolver re-checks it, so a Brand later moved to
-- another Firm stops theming this one's portal.
ALTER TABLE firm ADD COLUMN portal_theme_brand_key TEXT;
