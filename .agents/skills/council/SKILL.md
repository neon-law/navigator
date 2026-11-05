---
name: council
description: >-
  Review architecture, refactors, tests, documentation, or PR scope through engineering perspectives. Virgo chairs;
  render inline.
---

# Engineering Council

Read `docs/agent-decision-councils.md` and the relevant source before convening. Follow **Find the term first** in
`AGENTS.md`. Use the smallest useful bench; expand for an explicit full council or a broad decision. Councils render
inline as review lenses. Spawn subagents only when explicitly requested.

Virgo opens with the decision and closes with consensus and one action. Each other voice gives one concrete sentence or
passes. Cite exact files, symbols, or observed behavior. Keep the cast stable; use zodiac order when expanding.

| Voice | Role | Question |
| --- | --- | --- |
| Virgo | Engineering manager, chair | What decision and evidence define this review? |
| Aries | Incident commander | What is missing or broken? |
| Taurus | Production engineer | Where does the user encounter it? |
| Gemini | Integration engineer | Are words or interfaces carrying two contracts? |
| Cancer | New hire | What confuses a first-time reader? |
| Leo | Tech lead / DevRel | Can the rule be stated in one sentence? |
| Libra | Release manager | What is the smallest useful scope and sequence? |
| Scorpio | Security and trust engineer | Which assumption could break the trust boundary? |
| Sagittarius | Product manager | Who benefits and how does this serve the mission? |
| Capricorn | Lawyer engineer | Which current convention keeps this maintainable? |
| Aquarius | Platform engineer | Does an existing pattern or check cover this? |
| Pisces | Migration engineer | Which working behavior must remain intact? |

For test reviews, keep observable behavior, failure paths, and trust boundaries. Remove duplicate, self-referential,
fixture-only, and historical-copy checks. Preserve tests for active migrations and compatibility contracts.
