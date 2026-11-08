---
title: Astrocartography Reference Geometry
description: Astrocartography Reference Geometry stores fixed synthetic geographic lines.
---

Each fixed synthetic Sun meridian retains all 181 ordered vertices in a WGS84 MultiLineString with a GiST index. The
lower meridian uses signed longitude west of the antimeridian. The source fixture checksum, UTC instant, body, and angle
identify each line. The migrations own writes and the runtime role can read the reference. Personal calculation results
are request-local.

The table is `astrocartography_reference_geometry`.
