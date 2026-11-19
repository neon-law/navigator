# Privacy and recurring purchases

Navigator treats applicable privacy, consumer-protection, professional, and contractual duties as
[Covenants](glossary/covenant.md). These controls support those duties; they do not certify legal compliance.

## Account creation

The [Signup Age Screen](glossary/signup-age-screen.md) precedes password signup. Its signed decision also gates new
provider-created Persons. Existing Persons can sign in. An identity provider may share claims before an unknown user
reaches the screen; no Person is created by that refused callback. The screen does not verify identity or obtain
parental consent. It retains no submitted age or birth date.

## Fonts and analytics

Webfonts come from the site's own assets, including its configured asset origin. Google Fonts is not a runtime font
source. Public pages may load Plausible page analytics; authenticated matter pages do not. Navigator has no session
replay integration. The asset tests and content-security-policy tests guard the browser's external resource boundary.

## Email

Bundled email templates concern account access, a requested contact, or a workflow result. The welcome email links to
sign-in and contains no services promotion. Navigator has no automated marketing campaign sender.

A marketing sender needs a truthful sender and subject, advertisement identification, a valid postal address, a working
opt-out method, and suppression of opted-out recipients. Adding promotional copy to a transactional template changes
that assessment. Provider delivery alone does not supply these controls. See the [FTC's business
guide](https://www.ftc.gov/business-guidance/resources/can-spam-act-compliance-guide-business). Correspondence remains
draft-only for lawyer review under `AGENTS.md`.

## Recurring purchases

Recurring [Offerings](glossary/offering.md) use Stripe's hosted Checkout. Consultations remain invoiced in Xero.
Navigator supplies price and cadence from the catalog, shows renewal and cancellation terms beside its purchase control,
and requires a separate agreement. Stripe also displays those terms beside payment and requires terms acceptance. Its
customer invoice footer carries the same terms. The Purchase retains the accepted terms, consent time, Firm, and Stripe
account before Checkout starts.

One Person cannot open concurrent monthly and yearly Stripe subscriptions. Retries use the Purchase's idempotency key.
An overdue plan keeps its reservation until Stripe confirms renewal has stopped, and its online cancellation control
remains available. Pending legacy Xero plans also block a second recurring checkout until reconciled. An uncertain
attempt outside the safe retry window requires reconciliation. A provider redirect alone never activates access: server-
side checks confirm the Purchase reference, account, customer, payment, consent, price, and cadence. Access ends at the
last verified paid period. The durable payment schedule and the purchase and subscription pages refresh Stripe status; a
provider outage does not extend access.

The authenticated purchase page offers online cancellation. Cancellation requires the session's own Purchase, CSRF, and
provider confirmation. Stripe stops future renewals and preserves the paid period. Xero cancellation reads the current
period from its schedule before deleting future renewal and preserves access through that boundary. Existing invoices
and engagement records remain. Existing Xero renewal must be stopped before opening Stripe Checkout after its current
period ends; payment details and consent are not transferred automatically. Cancellation of billing does not end an
attorney-client relationship.

## Copyright notices

DMCA agent designation is an external registration, not something a code change can establish. An authorized operator
checks the [Copyright Office directory](https://www.copyright.gov/dmca-directory/), identifies each service provider's
legal entity and alternate names, registers the approved agent's public contact details, and pays the filing fee. The
operator verifies the public entry and publishes matching notice information on the provider's website. Keep
registration receipts and operational contact decisions in the approved private store, outside this repository.

The [Copyright Office's FAQ](https://www.copyright.gov/dmca-directory/faq.html) explains registration and renewal.
Registration is one condition of the relevant safe harbor; it does not establish all of its conditions. This repository
does not assert that an agent designation is registered or current.
