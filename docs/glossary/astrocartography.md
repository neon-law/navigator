---
title: Astrocartography
description: Astrocartography presents astrological angular lines over a geographic map for personal reflection.
---

## Astrocartography

Astrocartography presents astrological angular lines over a geographic map for personal reflection, relating a selected
instant to locations where celestial bodies rise, set, culminate, or reach the lower meridian. An
[Ephemeris](ephemeris.md) and explicit time and coordinate settings supply the inputs to a calculated map.

The design gallery uses public-domain land geometry and a calculated fixed reference, with explicit source provenance.
It demonstrates projection, line visibility, selection, and a text alternative. The reference does not use a person's
birth information and supplies no personal relocation advice.

## Canonical geographic geometry

`AstrocartographyGeometry::from_response` adapts selected exact-time [Astrology Chart](astrology-chart.md) provider
states into ordered WGS84 longitude/latitude segments, equivalent numerical point rows, and GeoJSON. It preserves body
selection and order, each physical body's SPK target ID, and the mean node's absent target ID. It uses the supplied
right ascension, declination, and precise instant without source I/O or body-position recalculation. Unknown and
approximate clocks return no exact-time geography.

The convention is a geocentric, unrefracted true geographic horizon at zero altitude, with no topocentric parallax. MC
and IC have local hour angles of zero and 180 degrees. ASC and DSC follow the altitude derivative under instantaneous
diurnal rotation at fixed right ascension and declination; tangent endpoints bound the crossing arcs. Horizon curves
omit circumpolar latitudes. Antimeridian crossings retain separate segments and paired boundary vertices. Physical pole
coordinates remain available; display projection and Mercator clipping are separate operations.

Provenance retains the exact source manifest and hashes, calculation and solver versions, settings, normalized
fractional calendar reference, and TT/TDB/UT1 readings. Native ICRF source coordinates and apparent true-of-date sky
directions are identified separately. The 0.000000001-degree tolerances describe emitted-vertex altitude and meridian
hour-angle residuals, separately from astronomical source accuracy.

The canonical solver refines horizon segments until every straight WGS84 longitude/latitude edge has a continuous
0.001-degree horizon angular-condition residual bound relative to the supplied body direction. This is the
coordinate-linear interpolation defined by [GeoJSON](https://www.rfc-editor.org/rfc/rfc7946#section-3.1.1), not a bound
on geographic displacement, geodesic paths, or a map provider's projected display. Meridians retain their constant
hour-angle condition. Original physical vertices, segment order, and paired antimeridian edges are preserved.

The continuous bound follows from a second-derivative remainder for sine altitude, with conservative unit coefficient
bounds and outward-inflated arithmetic. Provenance identifies the solver version, interpolation metric, bound and a
separate 0.000000000001 sine-altitude allowance for supported-platform floating-point trigonometry and conversion. This
allowance is distinct from source accuracy and is not formal verification of the platform math library. Refinement has
finite point/depth limits and fails closed if it cannot meet the bound or make numerical progress.

This is request-local calculated data, including a precise epoch, without the original birth input or birth location. A
basemap adapter receives only geographic segments and public body labels; precise private provenance stays on the
application side. Numerical rows follow the same body, angle, segment, and vertex order as GeoJSON, with zero-based
indices and no stored-chart identity.

## Mercator display adapter

The pure `webapp::astrocartography::mercator` adapter accepts canonical lines without the private epoch or calculation
provenance. It clips each straight geographic longitude/latitude edge to the square Mercator band at `atan(sinh(pi))`,
approximately 85.05112878 degrees north and south, inserting boundary intersections. It preserves source segment order,
paired antimeridian edges and separate exit/reentry subpaths; zero-length tangencies are omitted. Canonical physical
geometry remains unchanged, including its poles and continuous geographic condition bound.

The [Google world-coordinate convention](https://developers.google.com/maps/documentation/javascript/coordinates) uses
an eastward x and southward y from the northwest corner. The adapter normalizes the base world to 0–1 on each axis;
multiplication by 256 gives zoom-zero world pixels. Paired longitude edges remain x=0 and x=1 before an explicit integer
world offset. At most eight distinct world copies may be requested, a display resource limit rather than an astronomical
convention. Output carries body, source ID, angle and source indices, with no copied birth input or private provenance.
It has no debug formatting or serialization and activates no map provider, route, or rendered surface.

Geographic intersections follow coordinate-linear edges, not geodesics. A projected straight chord does not inherit the
canonical geographic horizon-condition certificate. This adapter defines clipping and coordinate conversion, without a
new astronomical accuracy, screen-space approximation or viewport-cropping guarantee. Projection roundoff at the square
boundary is checked within sixteen machine epsilons before its final 0–1 normalization.

The fixed Sun meridian is stored as [Astrocartography Reference Geometry](astrocartography-reference-geometry.md).
