---
title: "Signup Age Screen"
description: "The Signup Age Screen checks age before online account creation without saving the submitted age."
---

The Signup Age Screen asks a neutral age question before the email and password signup fields. An age of 13 through 120
permits signup; a response below 13 blocks it. The server signs only the eligibility decision and its expiry in a
30-minute cookie. The age itself is neither stored nor logged. A blocked browser cannot change its decision by reloading
or submitting an older age while that cookie remains present. Clearing cookies or using another browser can bypass this
self-reported screen; it is not identity verification or a parental-consent system.

Password signup checks the decision before calling the identity provider. New provider identities require it before
creating a Person; existing Persons can still sign in. An identity provider may already have shared claims with the
callback before an unknown identity is sent to the screen. No Person is created by that refused callback.

- Door: [`signup_age`](../../portal/src/signup_age.rs)
- Account creation: [`oauth`](../../portal/src/oauth.rs)
