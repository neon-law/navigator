---
title: "Booking Link"
description: >-
  A Booking Link is the address where a client books time with the firm, an appointment schedule saved to the Project
  Calendar.
---

`project.booking_url`: the booking page of a Google Calendar appointment schedule that saves bookings to the [Project
Calendar](project-calendar.md). The Calendar API has no resource for appointment schedules, so a lawyer creates the
schedule in Google Calendar on the web and records its address with `navigator project set --booking-url <address>`. The
steps are in the `project-setup` skill.

The link is shared: the matter page shows it to the client as "Book a time", as a [Resource](resource.md). It is
validated like every other resource link because it is rendered as an `href`. Closing the [Project](project.md) clears
it; `--booking-url ""` clears it by hand.
