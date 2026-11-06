---
title: Astrology Chart
description: An Astrology Chart presents celestial longitudes on a zodiac wheel with a text alternative.
---

## Astrology Chart

An Astrology Chart presents celestial longitudes on a zodiac wheel, with a text alternative listing each position.
[Astrology and Tarot](astrology-and-tarot.md) uses the chart for personal reflection. An [Ephemeris](ephemeris.md)
supplies calculated positions; the chart's calculation settings identify how those positions are interpreted.

The design gallery shows a calculated reference for 2000-01-01 at 12:00 UTC, at latitude and longitude zero. It collects
no birth information. Its pinned JPL source, calculation settings, body states, and generator identity travel with the
reference. The wheel and text tables show signed motion, house cusps, and selectable aspect relationships. Personal
calculations use the private Project calculation endpoint.

`POST /app/api/projects/{id}/astrology-chart` accepts birth inputs only in its JSON body, with explicit consent and
`personal_reflection` purpose for that calculation. A participating Client or lawyer-tier viewer may calculate. Clerks,
unrelated viewers, scoped machine sessions, and read-only client viewing sessions cannot calculate. Cookie requests
require the existing CSRF protection.

The endpoint uses verified local JPL sources and returns rendering values with calculation provenance. An unknown [Birth
Time](birth-time.md) supplies no assumed instant or positions; an approximate clock supplies no houses. Birth inputs and
results remain within request handling and its private response, outside the shared result cache and standing stores.
Responses, including refusals, use `private, no-store`. No saved chart or chart history is created; the caller controls
the lifetime of its response copy.

A Project can opt into the `astrology_chart` Section on an `estate_plan_console` Matter Page, separately for its
declared Client and Lawyer lenses. The private form starts blank and disabled until its matching page hydrates. It asks
for clock precision, explicit birth inputs, supported bodies and house convention, and consent for personal reflection.
It uses the same calculated wheel and position tables as the reference renderer. Read-only viewing and local Project
previews expose no calculation controls.

The form keeps its draft and result only in page-owned browser state. Editing, clearing, changing Project or leaving the
page discards them and invalidates any in-flight response. Page hiding synchronously hides personal controls and
results; a back-forward-cache restoration loads a fresh authorized blank page. No browser storage, chart history,
analytics or third-party map request receives personal chart values. Clearing releases application-owned values; it does
not promise secure erasure of browser memory or a response the reader has copied.
