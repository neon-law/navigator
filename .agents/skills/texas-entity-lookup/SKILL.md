---
name: texas-entity-lookup
description: >
  Verify a Texas business entity from the state's own records — legal name, SOS file number, formation state,
  registration status, registered agent, and the members, managers, or officers on its Public Information Report — and,
  for a real-estate broker, its TREC licence. Use whenever a matter needs a Texas counterparty's exact legal name, its
  members, its agent for service, or proof it may transact business in Texas.
---

# Look up a Texas entity

Texas SOSDirect charges per search and needs an account. The Comptroller's franchise-tax search is free, needs no
session, and carries the Secretary of State fields that matter — it is the route to take first.

## 1. Find the taxpayer number

```bash
curl -s 'https://comptroller.texas.gov/data-search/franchise-tax?name=EXAMPLE%20REALTY'
```

Search on a distinctive fragment, not the full legal name: the index matches on a prefix, and `EXAMPLE REALTY GROUP,
LLC` is missed by a query carrying the comma or the suffix. Two to fifty characters. `?taxpayerId=` and `?fileNumber=`
are the other two parameters, one at a time.

The reply is `{"success": true, "data": [{name, taxpayerId, mailingAddressZip}], "count": n}`. Match on the ZIP of the
address you already hold, because near-identical names in different Texas cities are the normal case.

## 2. Read the record

```bash
curl -s 'https://comptroller.texas.gov/data-search/franchise-tax/<taxpayerId>'
```

- `name`: The exact legal name, as the state spells it. This is what an instrument and a caption should carry.
- `sosFileNumber`: The Secretary of State filing number.
- `stateOfFormation`: Domestic or foreign. Arrives with a leading space — trim it.
- `sosRegistrationStatus`, `effectiveSosRegistrationDate`: Whether the filing is live, and since when.
- `rightToTransactTX`: `ACTIVE` here is the franchise-tax standing a Texas court asks about.
- `registeredAgentName`, `registeredOffice*`: Who accepts service, and where.
- `officerInfo[]`: `AGNT_NM` and `AGNT_TITL_TX` per person, with `SOURCE: SOS` and the report year.
- `dbaName`: Often null even where the entity trades under an assumed name; an assumed name lives on a separate filing.

`officerInfo` is how an LLC's membership is established from a public record. Two rows titled `MEMBER` make it
multi-member; one makes it single-member. `reportYear` says how current that is — it is last year's Public Information
Report, not today's cap table, so date the finding to the report year rather than to today.

## 3. Real-estate brokers, property managers, and leasing agents

Letting or managing property for an owner for a fee is brokerage in Texas, so the counterparty holds a TREC licence or
is acting without one — and either answer matters.

- The licence number is usually already in the instrument. A Texas listing or management agreement carries the
  `Information About Brokerage Services` (IABS) page, which names the licensed broker firm, its licence number, the
  **designated broker of the firm** and their own licence number, and any sponsoring supervisor and sales agent. Read
  that page before searching anything.
- `trec.texas.gov/license-search` is the register. Its search app redirects for scripted requests; `har.com` mirrors
  the same TREC rows and is readable through a fetch tool that renders pages.
- The designated broker is a named individual who answers for the firm's brokerage acts. Record them alongside the
  entity.

## 4. What none of this establishes

- **Assumed names.** `dba` on an instrument is a separate SOS filing; the Comptroller record usually does not carry
  it. Take the `dba` from the instrument.
- **Today's ownership.** `officerInfo` is a report-year snapshot.
- **Service of process.** A registered agent on this record is the state's copy; confirm against SOSDirect before
  serving.

Where the answer must be litigation-grade, this record is the lead and SOSDirect is the confirmation.

## 5. Recording it

An exact legal name from this record is verified against a public register, which is not the same as verified against
the instrument that names the counterparty. Where both exist, reconcile them and say which one a row was taken from. The
instrument governs who the contracting party is; this record governs how that party is spelled, and whether it may be
sued.
