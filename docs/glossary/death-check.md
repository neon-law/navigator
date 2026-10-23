---
title: "Death Check"
description: "A Death Check sets a Vesta estate plan's agreed triggers in motion when an active Nautilus client dies."
---

An optional service that sets the triggers in a [Vesta Estate Planning](vesta-estate-planning.md) estate plan in motion
when the client dies. The client and the firm write those triggers together while drafting the plan: who to tell, which
claims to file, such as a life insurance claim, and which accounts to close. The check runs only while the client holds
an active [Neon Law Nautilus](neon-law-nautilus.md) subscription, which is the same subscription as DeleteYourData.com,
and only if the client turns it on.

It is **client-English, not a schema noun** — like [Referral](referral.md), it names an offer a lawyer makes out loud,
not a table. The offer is authored as a `sibling_practices` card in the DeleteYourData.com home catalog
([`neon/locales/en/delete-your-data/home.yaml`](../../neon/locales/en/delete-your-data/home.yaml)) pointing at Vesta,
and Vesta's home carries the reciprocal card back to DeleteYourData.com. The copy promises the check, not any particular
probate, tax, insurance, or removal outcome.

## Why a life insurance claim needs a trigger

- **The insurer pays on proof, not on death.** A Nevada policy must provide that settlement "shall be made upon
  receipt of due proof of death" ([NRS 688A.140][nrs-688a-140]). Late proceeds accrue interest from the date of death
  ([NRS 688A.410][nrs-688a-410]).
- **The insurer's own search is periodic.** Nevada requires each insurer to compare its in-force policies against the
  Death Master File "at least semiannually" ([NRS 688D.090][nrs-688d-090]). A benefit whose beneficiary cannot be
  located is presumed abandoned ([NRS 688D.090][nrs-688d-090]).
- **Unidentified deaths leave beneficiaries unpaid.** A Florida appeals court found that when an insurer fails to
  identify a policyholder's death, "beneficiaries are not notified or paid benefits" ([*Patronis*][patronis] v. United
  Ins. Co. of Am., No. 1D18-2114 (Fla. 1st DCA June 3, 2020)). The opinion drew a dissent, and the citator showed no
  later treatment when this entry was written.

[nrs-688a-140]: <https://www.leg.state.nv.us/nrs/NRS-688A.html>
[nrs-688a-410]: <https://www.leg.state.nv.us/nrs/NRS-688A.html>
[nrs-688d-090]: <https://www.leg.state.nv.us/nrs/NRS-688D.html>
[patronis]: <https://app.midpage.ai/document/jimmy-patronis-in-his-official--b7fe6425-201e-44f5-93bb-980abef1299c>
