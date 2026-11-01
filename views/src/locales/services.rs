//! The firm's individual-services catalog: the structured twin of the prose
//! cards `/services` publishes.
//!
//! A marketing band is prose — a title and a paragraph. That is the right
//! shape for an argument and the wrong shape for a schedule of work: it
//! carries no identifier, no item number, no category, and no fee anything can
//! read. This module is the schedule, modelled as records, so the page can be
//! searched and filtered.
//!
//! It is the sibling of [`super::shared`] and deliberately wears the same
//! three guarantees, for the same reasons:
//!
//! - **Version.** [`ServicesCatalog::catalog_version`] is an explicit integer.
//!   A consumer built against a version it does not understand refuses the
//!   document rather than rendering half a fee schedule.
//! - **Closed vocabulary.** [`ServiceCategory`] is an enum, not a string, so a
//!   typo in a category becomes a validation error instead of a service that
//!   silently files under nothing.
//! - **Resolvable references.** A `related` id names a service this same
//!   catalog defines. A dangling one would render as a link to nowhere.
//!
//! One rule is this document's own. A service publishes **exactly one** a la
//! carte fee: either a [`FlatFee`] kind, which resolves to the catalog's single
//! [`ServicesCatalog::flat_fee`] figure, or a literal [`ServiceCopy::amount`].
//! Both is two a la carte prices on one matter; neither is a priced list with a
//! blank in it. Both are refused here rather than on the page. A Notation
//! package may additionally publish its lower price for a named plan.
//!
//! A catalog may instead declare `priced: false`: a directory of services the
//! firm quotes after a consultation. Then the rule inverts — no service
//! publishes any fee, so a page cannot print a price beside a list that says
//! it has none.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::shared;

/// The catalog version this build authors and understands.
pub const SUPPORTED_CATALOG_VERSION: u32 = 2;

/// The stem a services catalog file carries:
/// `locales/en/<brand-key>/services-catalog.yaml`.
///
/// Deliberately *not* in [`super::KNOWN_PAGES`]. That list is the stems a
/// brand renders as a page, and `BrandKey::catalog_pages` loads every one of
/// them as typed page copy. This document is a catalog a page reads, the way
/// `shared.yaml` is.
pub const SERVICES_CATALOG_STEM: &str = "services-catalog";

/// Which part of the practice a service belongs to.
///
/// An enum rather than a string: the vocabulary is closed, so an unknown value
/// is refused by name at parse time. The gallery's `all` is a filter control,
/// not a category a service can carry, and is absent for that reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCategory {
    /// Starting a business.
    Company,
    /// Keeping one running.
    Compliance,
    /// Names, marks, and the legal identity of a person or business.
    Identity,
    /// Wills, trusts, and family plans.
    Estate,
    /// Contracts and forms.
    Contracts,
    /// Disputes and housing — the work that arrives with a deadline.
    Urgent,
    /// Temporary work and talent visas: O-1, H-1B, L-1, TN, E-3, J-1.
    WorkVisas,
    /// Permanent residence: EB petitions, PERM, adjustment of status, and
    /// consular green cards.
    GreenCards,
    /// Work permits, travel documents, and dependent status: EAD, advance
    /// parole, STEM OPT, and I-539.
    PermitsAndTravel,
}

impl ServiceCategory {
    /// Every category, in the order a catalog declares them.
    pub const ALL: &'static [Self] = &[
        Self::Company,
        Self::Compliance,
        Self::Identity,
        Self::Estate,
        Self::Contracts,
        Self::Urgent,
        Self::WorkVisas,
        Self::GreenCards,
        Self::PermitsAndTravel,
    ];

    /// Words a service in this category should be findable by that a reader
    /// would never see printed.
    ///
    /// "Wills & family plans" is the firm's shelf label; a visitor types
    /// "family" or "legacy". These are searched and never rendered, so the
    /// page can meet a reader's word without publishing a word the firm would
    /// not choose.
    #[must_use]
    pub const fn search_aliases(self) -> &'static str {
        match self {
            Self::Estate => "personal family legacy",
            Self::WorkVisas => "job employer work visa",
            Self::GreenCards => "permanent residence green card",
            Self::PermitsAndTravel => "work permit travel renewal",
            _ => "",
        }
    }

    /// The value this category is written as in YAML and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Compliance => "compliance",
            Self::Identity => "identity",
            Self::Estate => "estate",
            Self::Contracts => "contracts",
            Self::Urgent => "urgent",
            Self::WorkVisas => "work_visas",
            Self::GreenCards => "green_cards",
            Self::PermitsAndTravel => "permits_and_travel",
        }
    }
}

/// Which flat fee a service charges.
///
/// Both kinds resolve to the catalog's single [`ServicesCatalog::flat_fee`]
/// figure. They stay distinct because the *disclosure* beside them differs —
/// a form fee is waived under a plan, a trademark filing is not — and because
/// collapsing them would make that difference unrecoverable from the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlatFee {
    /// The per-form fee.
    Form,
    /// The trademark filing fee, which a government charge is added to.
    Trademark,
}

/// One category and the words a reader sees for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryCopy {
    /// What a reader sees.
    pub label: String,
    /// Which category this names.
    pub value: ServiceCategory,
}

/// One service the firm publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceCopy {
    /// A literal fee, e.g. `$350`. Exactly one of this and `flat_fee` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    /// One sentence or two saying what the work is, in a reader's words.
    pub blurb: String,
    pub category: ServiceCategory,
    /// The flat-fee kind. Exactly one of this and `amount` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat_fee: Option<FlatFee>,
    /// The stable identifier a `related` reference and a deep link name.
    pub id: String,
    /// What the fee buys, as scope lines. A service with no scope reads as
    /// covering everything.
    #[serde(default)]
    pub includes: Vec<String>,
    /// The numeric item code, kept as a string so a leading zero survives.
    pub item: String,
    /// Extra words a reader might search by that the name and blurb do not
    /// already contain.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Whether this service requires a plan.
    #[serde(default)]
    pub members_only: bool,
    pub name: String,
    /// When set, this service is a Notation package: several pieces of work
    /// sold together for less than buying each included Notation on its own.
    /// The package fee is this service's own fee; the à la carte total is
    /// derived from [`ServicePackage::of`] and [`ServicePackage::extras`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<ServicePackage>,
    /// What the fee is charged per — `per form`, `per contract`, `per year`.
    /// Required in a priced catalog; absent in an unpriced one.
    #[serde(default)]
    pub period: String,
    /// The discounted price a named plan pays for this Notation package.
    /// Validation requires it to be at least 50% below the a la carte price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_price: Option<PlanPrice>,
    /// Other services in this catalog worth reading next.
    #[serde(default)]
    pub related: Vec<String>,
    /// Whether a government body charges its own fee on top of this one.
    #[serde(default)]
    pub state_fee: bool,
    /// The notation template a public start can open for this service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Copy shared by a service card's start link and the start-door flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StartDoorCopy {
    pub confirmation: String,
    pub disclosure: String,
    pub label: String,
    pub microcopy: String,
    pub refusal: String,
}

/// How often an [`OfferingCopy`] is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OfferingCadence {
    #[default]
    Once,
    Month,
    Year,
}

/// Something a signed-in Client may buy online. Xero invoices it; the
/// Purchase turns on once the invoice is paid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OfferingCopy {
    /// Stable identifier, recorded on each Purchase and used in its URL.
    pub id: String,
    pub name: String,
    pub blurb: String,
    /// The published price, a dollar figure such as `$29`.
    pub amount: String,
    pub cadence: OfferingCadence,
    /// The Xero chart-of-accounts code the revenue posts to.
    pub account_code: String,
    /// Waived for a Client who signed in with Google using their address.
    #[serde(default)]
    pub complimentary_with_google: bool,
}

impl OfferingCopy {
    /// The price in cents, `None` when [`Self::amount`] is not a fee.
    #[must_use]
    pub fn amount_cents(&self) -> Option<i64> {
        fee_cents(&self.amount)
    }
}

/// The purchase page's copy and the Offerings it sells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PurchaseCopy {
    pub title: String,
    pub intro: String,
    /// Shown to a Client whose consultation is complimentary.
    pub complimentary: String,
    /// Shown to every other Client beside a complimentary-with-Google Offering.
    pub google_hint: String,
    pub book_label: String,
    pub buy_label: String,
    pub pay_label: String,
    /// Shown beside a Purchase whose invoice is unpaid.
    pub invoiced: String,
    /// Shown beside a Purchase in force.
    pub active: String,
    pub refusal: String,
    pub offerings: Vec<OfferingCopy>,
}

impl ServiceCopy {
    /// Every value of this service that a reader can see or search.
    ///
    /// Used by validation to check each one is substitutable, and by the
    /// renderer to build a search haystack, so the two cannot disagree about
    /// what counts as published copy.
    fn values(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("blurb", self.blurb.as_str()),
            ("name", self.name.as_str()),
            ("item", self.item.as_str()),
        ]
        .into_iter()
        .chain((!self.period.is_empty()).then_some(("period", self.period.as_str())))
        .chain(self.amount.as_deref().map(|amount| ("amount", amount)))
        .chain(self.includes.iter().map(|line| ("includes", line.as_str())))
        .chain(self.keywords.iter().map(|word| ("keywords", word.as_str())))
        .chain(
            self.plan_price
                .iter()
                .map(|price| ("plan_price.plan", price.plan.as_str())),
        )
        .chain(self.package.iter().flat_map(|package| {
            package
                .extras
                .iter()
                .map(|extra| ("package.extras.name", extra.name.as_str()))
        }))
    }
}

/// A package's price for a named plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPrice {
    pub amount: String,
    pub plan: String,
}

/// One named piece of work a package includes that is not sold as its own
/// catalog service. Its amount is the à la carte starting fee used to
/// compute the package comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageExtra {
    pub amount: String,
    pub name: String,
}

/// The members of a Notation package.
///
/// [`Self::of`] names other services in this catalog; [`Self::extras`] names
/// work the package includes that has no row of its own. Together they prove
/// the package's a la carte fee remains lower than buying its pieces one at a
/// time. A plan price is a separate, validated offer on the package itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ServicePackage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<PackageExtra>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub of: Vec<String>,
}

/// The included Notations a package publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageQuote {
    /// The names of the included Notations, in publication order: catalog
    /// members first, then extras.
    pub members: Vec<String>,
}

/// The services catalog document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicesCatalog {
    /// The contract version. See [`SUPPORTED_CATALOG_VERSION`].
    pub catalog_version: u32,
    /// The categories, in publication order, each with its reader-facing
    /// label.
    pub categories: Vec<CategoryCopy>,
    /// What a [`FlatFee`] service costs. One figure, so a page cannot print
    /// two different "flat fees". Required in a priced catalog; refused in an
    /// unpriced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat_fee: Option<String>,
    /// Whether this catalog publishes fees. `false` is a directory of services
    /// quoted after a consultation: no service carries an amount.
    #[serde(default = "default_priced")]
    pub priced: bool,
    /// The services, in publication order.
    pub services: Vec<ServiceCopy>,
    /// The copy for the service start door and the client intake confirmation.
    #[serde(default)]
    pub start: StartDoorCopy,
    /// The copy for the purchase page and the Offerings it sells.
    #[serde(default)]
    pub purchase: PurchaseCopy,
}

impl ServicesCatalog {
    /// Deserialize and validate one services catalog document.
    pub fn parse(yaml: &str) -> Result<Self, String> {
        let catalog: Self =
            serde_yaml::from_str(yaml).map_err(|err| format!("{SERVICES_CATALOG_STEM}: {err}"))?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn validate(&self) -> Result<(), String> {
        if self.catalog_version != SUPPORTED_CATALOG_VERSION {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: catalog version {} is not supported; this build authors \
                 version {SUPPORTED_CATALOG_VERSION}",
                self.catalog_version
            ));
        }
        match (self.priced, self.flat_fee.as_deref()) {
            (true, Some(flat_fee)) => check_fee("flat_fee", flat_fee)?,
            (true, None) => {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: a priced catalog publishes `flat_fee`"
                ))
            }
            (false, Some(_)) => {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: an unpriced catalog publishes no `flat_fee`"
                ))
            }
            (false, None) => {}
        }
        self.validate_categories()?;
        self.validate_start()?;
        self.validate_purchase()?;
        let ids = self.validate_services()?;
        for service in &self.services {
            for related in &service.related {
                if !ids.contains(related.as_str()) {
                    return Err(format!(
                        "{SERVICES_CATALOG_STEM}: `{}` is related to `{related}`, which this \
                         catalog does not define",
                        service.id
                    ));
                }
                if related == &service.id {
                    return Err(format!(
                        "{SERVICES_CATALOG_STEM}: `{}` is related to itself",
                        service.id
                    ));
                }
            }
            if service.package.is_some() {
                self.validate_package(service, &ids)?;
            }
            self.validate_plan_price(service)?;
        }
        Ok(())
    }

    fn validate_start(&self) -> Result<(), String> {
        if self.start == StartDoorCopy::default() {
            return Ok(());
        }
        for (key, value) in [
            ("start.confirmation", self.start.confirmation.as_str()),
            ("start.disclosure", self.start.disclosure.as_str()),
            ("start.label", self.start.label.as_str()),
            ("start.microcopy", self.start.microcopy.as_str()),
            ("start.refusal", self.start.refusal.as_str()),
        ] {
            check_value(key, value)?;
        }
        Ok(())
    }

    fn validate_purchase(&self) -> Result<(), String> {
        if self.purchase == PurchaseCopy::default() {
            return Ok(());
        }
        let copy = &self.purchase;
        for (key, value) in [
            ("purchase.title", copy.title.as_str()),
            ("purchase.intro", copy.intro.as_str()),
            ("purchase.complimentary", copy.complimentary.as_str()),
            ("purchase.google_hint", copy.google_hint.as_str()),
            ("purchase.book_label", copy.book_label.as_str()),
            ("purchase.buy_label", copy.buy_label.as_str()),
            ("purchase.pay_label", copy.pay_label.as_str()),
            ("purchase.invoiced", copy.invoiced.as_str()),
            ("purchase.active", copy.active.as_str()),
            ("purchase.refusal", copy.refusal.as_str()),
        ] {
            check_value(key, value)?;
        }
        let mut ids = BTreeSet::new();
        for offering in &copy.offerings {
            let id = offering.id.as_str();
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: offering id `{id}` is not lowercase letters, digits, \
                     and hyphens"
                ));
            }
            if !ids.insert(id) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: offering `{id}` is declared twice"
                ));
            }
            check_value(&format!("purchase.offerings.{id}.name"), &offering.name)?;
            check_value(&format!("purchase.offerings.{id}.blurb"), &offering.blurb)?;
            check_fee(&format!("purchase.offerings.{id}.amount"), &offering.amount)?;
            if offering.account_code.is_empty()
                || !offering
                    .account_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
            {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: offering `{id}` needs a Xero account code"
                ));
            }
        }
        Ok(())
    }

    /// Every category is declared at most once, and every category a service
    /// names is declared.
    ///
    /// A service whose category is undeclared is one the page cannot label; a
    /// catalog that declares one twice has two labels for the same shelf and no
    /// rule for which wins. A category no service names may be left out, so
    /// each brand publishes only its own shelves.
    fn validate_categories(&self) -> Result<(), String> {
        let mut seen: BTreeSet<ServiceCategory> = BTreeSet::new();
        for category in &self.categories {
            check_value(category.value.as_str(), &category.label)?;
            if !seen.insert(category.value) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: category `{}` is declared twice",
                    category.value.as_str()
                ));
            }
        }
        for category in self.services.iter().map(|service| &service.category) {
            if !seen.contains(category) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: category `{}` has no label",
                    category.as_str()
                ));
            }
        }
        Ok(())
    }

    /// Check each service and return the set of ids, for the `related` pass.
    fn validate_services(&self) -> Result<BTreeSet<&str>, String> {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        let mut items: BTreeSet<&str> = BTreeSet::new();
        for service in &self.services {
            if !ids.insert(service.id.as_str()) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` is defined twice",
                    service.id
                ));
            }
            if !items.insert(service.item.as_str()) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: item number `{}` is used twice, most recently by \
                     `{}`",
                    service.item, service.id
                ));
            }
            if !self.priced {
                Self::validate_unpriced(service)?;
            }
            match (&service.flat_fee, &service.amount) {
                _ if !self.priced => {}
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "{SERVICES_CATALOG_STEM}: `{}` publishes both a flat fee and an amount; a \
                         matter carries one fee",
                        service.id
                    ))
                }
                (None, None) => {
                    return Err(format!(
                        "{SERVICES_CATALOG_STEM}: `{}` publishes no fee; set `flat_fee` or \
                         `amount`",
                        service.id
                    ))
                }
                _ => {}
            }
            if let Some(amount) = service.amount.as_deref() {
                check_fee(&format!("{}.amount", service.id), amount)?;
            }
            if self.priced && service.period.trim().is_empty() {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` publishes a fee without saying what it is \
                     charged per; set `period`",
                    service.id
                ));
            }
            if service.includes.is_empty() {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` names no scope, which reads as covering \
                     everything",
                    service.id
                ));
            }
            for (field, value) in service.values() {
                check_value(&format!("{}.{field}", service.id), value)?;
            }
        }
        Ok(ids)
    }

    /// The words a reader sees for `category`.
    ///
    /// Infallible for a category a service names: [`Self::validate_categories`]
    /// proves each one carries a label before a catalog exists. A category no
    /// service names may be undeclared, and reads as empty.
    #[must_use]
    pub fn category_label(&self, category: ServiceCategory) -> &str {
        self.categories
            .iter()
            .find(|declared| declared.value == category)
            .map_or("", |declared| declared.label.as_str())
    }

    /// The service `id` names, if this catalog defines it.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ServiceCopy> {
        self.services.iter().find(|service| service.id == id)
    }

    /// Refuse any fee on a service in an unpriced catalog.
    fn validate_unpriced(service: &ServiceCopy) -> Result<(), String> {
        let published = [
            ("amount", service.amount.is_some()),
            ("flat_fee", service.flat_fee.is_some()),
            ("period", !service.period.is_empty()),
            ("plan_price", service.plan_price.is_some()),
            ("package", service.package.is_some()),
        ];
        match published.iter().find(|(_, set)| *set) {
            Some((field, _)) => Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` sets `{field}` in an unpriced catalog, which \
                 publishes no fees",
                service.id
            )),
            None => Ok(()),
        }
    }

    /// What `service` charges: its literal amount, or the catalog's one flat
    /// fee. `None` only in an unpriced catalog; validation proves every
    /// service in a priced one resolves.
    #[must_use]
    pub fn fee<'a>(&'a self, service: &'a ServiceCopy) -> Option<&'a str> {
        service.amount.as_deref().or_else(|| {
            service
                .flat_fee
                .and(self.flat_fee.as_deref())
        })
    }

    /// The included Notations for a Notation package, if `service` is one.
    ///
    /// Infallible for a catalog that has passed [`Self::parse`]: validation
    /// has already proved every member exists, every fee is a payable
    /// figure, and the package costs less than buying the members one at a
    /// time.
    #[must_use]
    pub fn package_quote(&self, service: &ServiceCopy) -> Option<PackageQuote> {
        let package = service.package.as_ref()?;
        let mut members = Vec::new();
        for id in &package.of {
            let member = self.get(id)?;
            members.push(member.name.clone());
        }
        for extra in &package.extras {
            members.push(extra.name.clone());
        }
        Some(PackageQuote { members })
    }

    fn validate_plan_price(&self, service: &ServiceCopy) -> Result<(), String> {
        let Some(plan_price) = service.plan_price.as_ref() else {
            return Ok(());
        };
        if service.package.is_none() {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` publishes a plan price but is not a Notation package",
                service.id
            ));
        }
        check_fee(
            &format!("{}.plan_price.amount", service.id),
            &plan_price.amount,
        )?;
        let Some(a_la_carte_cents) = self.fee(service).and_then(fee_cents) else {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` has no payable a la carte fee",
                service.id
            ));
        };
        let Some(plan_cents) = fee_cents(&plan_price.amount) else {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}.plan_price.amount` has no payable fee",
                service.id
            ));
        };
        if plan_cents.saturating_mul(2) > a_la_carte_cents {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` plan price is not at least half off its a la carte fee",
                service.id
            ));
        }
        Ok(())
    }

    fn validate_package(&self, service: &ServiceCopy, ids: &BTreeSet<&str>) -> Result<(), String> {
        let Some(package) = service.package.as_ref() else {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` has no package to validate",
                service.id
            ));
        };
        if package.of.is_empty() && package.extras.is_empty() {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` is a package with no members; set `package.of` or \
                 `package.extras`",
                service.id
            ));
        }
        if package.of.len() + package.extras.len() < 2 {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` is a package of one Notation; a package compares \
                 at least two",
                service.id
            ));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut separate_cents: i64 = 0;
        for id in &package.of {
            if id == &service.id {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` lists itself as a package member",
                    service.id
                ));
            }
            if !ids.contains(id.as_str()) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` is a package of `{id}`, which this catalog \
                     does not define",
                    service.id
                ));
            }
            if !seen.insert(id.as_str()) {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` lists `{id}` twice as a package member",
                    service.id
                ));
            }
            let Some(member) = self.get(id) else {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` is a package of `{id}`, which this catalog \
                     does not define",
                    service.id
                ));
            };
            if member.package.is_some() {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}` is a package of `{id}`, which is itself a \
                     package",
                    service.id
                ));
            }
            let Some(member_cents) = self.fee(member).and_then(fee_cents) else {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}.package.of` member `{id}` has no payable fee",
                    service.id
                ));
            };
            separate_cents = separate_cents.saturating_add(member_cents);
        }
        for (index, extra) in package.extras.iter().enumerate() {
            check_fee(
                &format!("{}.package.extras[{index}].amount", service.id),
                &extra.amount,
            )?;
            let Some(extra_cents) = fee_cents(&extra.amount) else {
                return Err(format!(
                    "{SERVICES_CATALOG_STEM}: `{}.package.extras[{index}].amount` has no payable \
                     fee",
                    service.id
                ));
            };
            separate_cents = separate_cents.saturating_add(extra_cents);
        }
        let Some(package_cents) = self.fee(service).and_then(fee_cents) else {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` has no payable package fee",
                service.id
            ));
        };
        if separate_cents <= package_cents {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{}` does not cost less than buying each included \
                 Notation on its own",
                service.id
            ));
        }
        Ok(())
    }
}

/// Refuse a value a page cannot publish.
///
/// The same three rules [`super::shared`] applies, and for the same reasons: a
/// value is substituted into a quoted scalar, so a newline or a double quote
/// breaks the document it lands in, and a placeholder outside the supported
/// set reaches a reader as a literal brace.
fn check_value(key: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{SERVICES_CATALOG_STEM}: `{key}` is empty"));
    }
    if value.contains('\n') {
        return Err(format!(
            "{SERVICES_CATALOG_STEM}: `{key}` spans more than one line; a catalog value is \
             substituted into a quoted scalar"
        ));
    }
    if value.contains('"') {
        return Err(format!(
            "{SERVICES_CATALOG_STEM}: `{key}` contains a double quote, which cannot be substituted \
             into a quoted scalar"
        ));
    }
    for placeholder in shared::placeholders(value) {
        if !shared::SUPPORTED_PLACEHOLDERS.contains(&placeholder.as_str()) {
            return Err(format!(
                "{SERVICES_CATALOG_STEM}: `{key}` uses unsupported placeholder \
                 `{{{placeholder}}}`; expected one of {}",
                shared::SUPPORTED_PLACEHOLDERS.join(", ")
            ));
        }
    }
    Ok(())
}

fn default_priced() -> bool {
    true
}

/// Refuse a published fee that is not a figure a reader can pay.
///
/// [`check_value`] asks only whether a string is safe to substitute. A fee is
/// advertised to the public and has to clear more than that: `$0` on a legal
/// fee schedule reads as free work the firm is not offering, `-$50` is not a
/// price at all, and `free` is a claim rather than an amount — yet all three
/// satisfy every text rule and would reach the page.
///
/// Positivity is decided by looking for a non-zero digit rather than by
/// parsing. A fee is money, and money is the wrong thing to route through a
/// binary float on its way to a public page.
fn check_fee(key: &str, value: &str) -> Result<(), String> {
    check_value(key, value)?;
    let refuse = |reason: &str| {
        Err(format!(
            "{SERVICES_CATALOG_STEM}: `{key}` publishes `{value}`, which {reason}"
        ))
    };
    let Some(amount) = value.strip_prefix('$') else {
        return refuse("is not a fee; a published fee is a dollar figure such as `$350`");
    };
    let (whole, cents) = match amount.split_once('.') {
        Some((whole, cents)) => (whole, Some(cents)),
        None => (amount, None),
    };
    if whole.is_empty()
        || !whole
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b',')
    {
        return refuse(
            "is not a fee; the amount is digits, optionally grouped with commas, such as `$3,650`",
        );
    }
    if let Some(cents) = cents {
        if cents.len() != 2 || !cents.bytes().all(|byte| byte.is_ascii_digit()) {
            return refuse("is not a fee; cents are exactly two digits, such as `$12.50`");
        }
    }
    if !amount.bytes().any(|byte| matches!(byte, b'1'..=b'9')) {
        return refuse("is not a fee a reader can pay; a published fee is greater than zero");
    }
    Ok(())
}

/// Integer minor units for a fee that has already passed [`check_fee`].
///
/// Commas are grouping, not a decimal. Cents, when present, are exactly two
/// digits. The return is `None` only for a string [`check_fee`] would already
/// have refused.
fn fee_cents(value: &str) -> Option<i64> {
    let amount = value.strip_prefix('$')?;
    let compact: String = amount
        .chars()
        .filter(|character| *character != ',')
        .collect();
    let (whole, frac) = match compact.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (compact.as_str(), "00"),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || frac.len() != 2
        || !frac.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let dollars: i64 = whole.parse().ok()?;
    let cents: i64 = frac.parse().ok()?;
    Some(dollars.saturating_mul(100).saturating_add(cents))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_purchase(amount: &str, id: &str, account_code: &str) -> String {
        format!(
            "{}\npurchase:\n  title: Plans\n  intro: Buy here.\n  complimentary: Free.\n  \
             google_hint: Use Google.\n  book_label: Book\n  buy_label: Buy\n  pay_label: Pay\n  \
             invoiced: Sent.\n  active: Active\n  refusal: Not now.\n  offerings:\n    - id: {id}\n      \
             name: Plan\n      blurb: A plan.\n      amount: \"{amount}\"\n      cadence: month\n      \
             account_code: \"{account_code}\"\n",
            fixture()
        )
    }

    #[test]
    fn an_offering_needs_a_positive_published_price_an_id_and_an_account_code() {
        let catalog = ServicesCatalog::parse(&with_purchase("$29", "plan-monthly", "200")).unwrap();
        assert_eq!(catalog.purchase.offerings[0].amount_cents(), Some(2_900));
        assert_eq!(
            catalog.purchase.offerings[0].cadence,
            OfferingCadence::Month
        );
        for (amount, id, code, refusal) in [
            ("$0", "plan", "200", "greater than zero"),
            ("29", "plan", "200", "is not a fee"),
            ("$29", "Plan!", "200", "lowercase"),
            ("$29", "plan", "", "account code"),
        ] {
            let error = ServicesCatalog::parse(&with_purchase(amount, id, code)).unwrap_err();
            assert!(error.contains(refusal), "{amount} {id} {code}: {error}");
        }
    }

    #[test]
    fn the_bundled_catalog_publishes_its_offerings() {
        let catalog = ServicesCatalog::parse(include_str!(
            "../../../neon/locales/en/neon/services-catalog.yaml"
        ))
        .unwrap();
        assert_eq!(catalog.purchase.offerings.len(), 3);
    }

    /// A two-service catalog with every required field, which each test below
    /// mutates in exactly one way.
    fn fixture() -> String {
        String::from(
            r"catalog_version: 2
flat_fee: $50
categories:
  - value: company
    label: Start a business
  - value: compliance
    label: Run a business
  - value: identity
    label: Names and brands
  - value: estate
    label: Wills and family plans
  - value: contracts
    label: Contracts and forms
  - value: urgent
    label: Disputes and housing
services:
  - id: llc-file
    template: nv__llc_formation
    item: '1101'
    name: Start a company
    blurb: We set up your company and a lawyer files the papers.
    category: company
    flat_fee: form
    period: per form
    state_fee: true
    includes:
      - Prepare the papers
    related:
      - nv-address
    keywords:
      - LLC
  - id: nv-address
    item: '1202'
    name: Nevada business address
    blurb: A Nevada street address for legal notices.
    category: compliance
    amount: $350
    period: per year
    state_fee: false
    includes:
      - A street address
",
        )
    }

    #[test]
    fn the_fixture_parses() {
        let catalog = ServicesCatalog::parse(&fixture()).expect("the fixture is valid");
        assert_eq!(catalog.services.len(), 2);
        assert_eq!(
            catalog.category_label(ServiceCategory::Company),
            "Start a business"
        );
        // A flat-fee service reads the catalog's one figure; a literal amount
        // reads itself.
        let llc = catalog.get("llc-file").expect("llc-file");
        assert_eq!(llc.template.as_deref(), Some("nv__llc_formation"));
        assert_eq!(catalog.fee(llc), Some("$50"));
        let address = catalog.get("nv-address").expect("nv-address");
        assert_eq!(catalog.fee(address), Some("$350"));
        assert!(catalog.get("nothing").is_none());
    }

    /// The fixture with every fee stripped and the catalog marked unpriced.
    fn unpriced_fixture() -> String {
        fixture()
            .replace("flat_fee: $50\n", "priced: false\n")
            .replace("    flat_fee: form\n", "")
            .replace("    amount: $350\n", "")
            .replace("    period: per form\n", "")
            .replace("    period: per year\n", "")
    }

    #[test]
    fn an_unpriced_catalog_publishes_no_fee() {
        let catalog = ServicesCatalog::parse(&unpriced_fixture()).expect("unpriced is valid");
        assert!(!catalog.priced);
        assert!(catalog.flat_fee.is_none());
        for service in &catalog.services {
            assert_eq!(catalog.fee(service), None, "{}", service.id);
        }
    }

    #[test]
    fn an_unpriced_catalog_refuses_any_fee() {
        let with_amount = unpriced_fixture().replace(
            "    category: compliance\n",
            "    category: compliance\n    amount: $350\n",
        );
        let err = ServicesCatalog::parse(&with_amount).expect_err("amount in unpriced");
        assert!(
            err.contains("`nv-address` sets `amount` in an unpriced catalog"),
            "{err}"
        );
        let with_flat_fee = unpriced_fixture().replace("priced: false\n", "priced: false\nflat_fee: $50\n");
        let err = ServicesCatalog::parse(&with_flat_fee).expect_err("flat fee in unpriced");
        assert!(err.contains("an unpriced catalog publishes no `flat_fee`"), "{err}");
    }

    #[test]
    fn a_priced_catalog_requires_its_flat_fee() {
        let err = ServicesCatalog::parse(&fixture().replace("flat_fee: $50\n", ""))
            .expect_err("no flat fee");
        assert!(err.contains("a priced catalog publishes `flat_fee`"), "{err}");
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace("id: nv-address", "id: llc-file"))
            .expect_err("duplicate id");
        assert!(err.contains("`llc-file` is defined twice"), "{err}");
    }

    #[test]
    fn a_duplicate_item_number_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace("item: '1202'", "item: '1101'"))
            .expect_err("duplicate item");
        assert!(err.contains("item number `1101` is used twice"), "{err}");
    }

    /// The vocabulary is closed, so an unknown category is named and the
    /// accepted ones are listed — serde's own unknown-variant error.
    #[test]
    fn an_unknown_category_is_refused() {
        let err =
            ServicesCatalog::parse(&fixture().replace("category: company", "category: crypto"))
                .expect_err("unknown category");
        assert!(err.contains("crypto"), "{err}");
        assert!(err.contains("company"), "{err}");
    }

    #[test]
    fn a_category_declared_twice_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace(
            "  - value: compliance\n    label: Run a business",
            "  - value: company\n    label: Run a business",
        ))
        .expect_err("duplicate category");
        assert!(
            err.contains("category `company` is declared twice"),
            "{err}"
        );
    }

    #[test]
    fn a_category_a_service_names_with_no_label_is_refused() {
        let err = ServicesCatalog::parse(
            &fixture().replace("  - value: compliance\n    label: Run a business\n", ""),
        )
        .expect_err("missing category label");
        assert!(err.contains("category `compliance` has no label"), "{err}");
    }

    /// Each brand publishes its own shelves: an immigration catalog does not
    /// label "Start a business", and a business catalog does not label
    /// "Green cards".
    #[test]
    fn a_category_no_service_names_may_be_left_out() {
        let catalog = ServicesCatalog::parse(
            &fixture().replace("  - value: urgent\n    label: Disputes and housing\n", ""),
        )
        .expect("an unused category needs no label");
        assert_eq!(catalog.category_label(ServiceCategory::Urgent), "");
    }

    #[test]
    fn an_immigration_catalog_parses_with_only_its_own_categories() {
        let catalog = ServicesCatalog::parse(
            r"catalog_version: 2
flat_fee: $50
categories:
  - value: work_visas
    label: Work visas
  - value: green_cards
    label: Green cards
  - value: permits_and_travel
    label: Permits and travel
services:
  - id: o1-new
    item: '2101'
    name: O-1 petition
    blurb: A petition for a person with extraordinary ability.
    category: work_visas
    amount: $7,200
    period: per petition
    state_fee: true
    includes:
      - Prepare and file the petition
",
        )
        .expect("an immigration catalog is valid");
        assert_eq!(
            catalog.category_label(ServiceCategory::WorkVisas),
            "Work visas"
        );
    }

    #[test]
    fn a_dangling_related_id_is_refused() {
        let err =
            ServicesCatalog::parse(&fixture().replace("      - nv-address\n", "      - trust\n"))
                .expect_err("dangling related");
        assert!(
            err.contains("`llc-file` is related to `trust`, which this catalog does not define"),
            "{err}"
        );
    }

    #[test]
    fn a_service_related_to_itself_is_refused() {
        let err = ServicesCatalog::parse(
            &fixture().replace("      - nv-address\n", "      - llc-file\n"),
        )
        .expect_err("self-related");
        assert!(err.contains("`llc-file` is related to itself"), "{err}");
    }

    #[test]
    fn a_stray_placeholder_is_refused() {
        let err = ServicesCatalog::parse(
            &fixture().replace("period: per year", "period: per year, call {firm_phone}"),
        )
        .expect_err("stray placeholder");
        assert!(err.contains("{firm_phone}"), "{err}");
        assert!(err.contains("nv-address.period"), "{err}");
    }

    /// The two brand placeholders are supported, so a catalog may use them.
    #[test]
    fn the_brand_placeholders_are_accepted() {
        ServicesCatalog::parse(&fixture().replace(
            "blurb: A Nevada street address for legal notices.",
            "blurb: A Nevada street address, forwarded by {site_name} to {firm_email}.",
        ))
        .expect("brand placeholders are supported");
    }

    #[test]
    fn a_value_spanning_two_lines_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace(
            "blurb: A Nevada street address for legal notices.",
            "blurb: |\n      A Nevada street address.\n      Forwarded to you.",
        ))
        .expect_err("multi-line value");
        assert!(err.contains("spans more than one line"), "{err}");
    }

    #[test]
    fn an_empty_value_is_refused() {
        let err = ServicesCatalog::parse(
            &fixture().replace("name: Nevada business address", "name: ' '"),
        )
        .expect_err("empty value");
        assert!(err.contains("`nv-address.name` is empty"), "{err}");
    }

    #[test]
    fn publishing_two_fees_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace(
            "    amount: $350\n",
            "    amount: $350\n    flat_fee: form\n",
        ))
        .expect_err("two fees");
        assert!(
            err.contains("`nv-address` publishes both a flat fee and an amount"),
            "{err}"
        );
    }

    #[test]
    fn publishing_no_fee_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace("    amount: $350\n", ""))
            .expect_err("no fee");
        assert!(err.contains("`nv-address` publishes no fee"), "{err}");
    }

    /// A published fee has to be a figure a reader can pay. These all satisfy
    /// every text rule — one line, no quotes, no stray placeholder — and would
    /// otherwise reach the public fee schedule.
    #[test]
    fn a_fee_that_is_not_a_payable_figure_is_refused() {
        for (amount, expected) in [
            ("$0", "greater than zero"),
            ("$0.00", "greater than zero"),
            ("$000", "greater than zero"),
            ("-$50", "is not a fee"),
            ("$-50", "is not a fee"),
            ("free", "is not a fee"),
            ("350", "is not a fee"),
            ("$35.0", "cents are exactly two digits"),
            ("$35.000", "cents are exactly two digits"),
            ("$", "is not a fee"),
        ] {
            let err = ServicesCatalog::parse(
                &fixture().replace("amount: $350", &format!("amount: '{amount}'")),
            )
            .unwrap_err();
            assert!(
                err.contains("nv-address.amount") && err.contains(expected),
                "`{amount}` must be refused as a fee mentioning {expected:?}: {err}"
            );
        }
    }

    #[test]
    fn a_fee_a_reader_can_pay_is_accepted() {
        for amount in ["$5", "$350", "$3,650", "$12.50", "$0.50"] {
            ServicesCatalog::parse(
                &fixture().replace("amount: $350", &format!("amount: '{amount}'")),
            )
            .unwrap_or_else(|err| panic!("`{amount}` is a payable fee: {err}"));
        }
    }

    /// The catalog-level flat fee is held to the same rule: it is the figure
    /// every flat-fee service prints.
    #[test]
    fn a_flat_fee_that_is_not_a_payable_figure_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace("flat_fee: $50", "flat_fee: '$0'"))
            .expect_err("a zero flat fee");
        assert!(err.contains("`flat_fee`"), "{err}");
        assert!(err.contains("greater than zero"), "{err}");
    }

    #[test]
    fn a_service_with_no_scope_is_refused() {
        let err = ServicesCatalog::parse(
            &fixture().replace("    includes:\n      - A street address\n", ""),
        )
        .expect_err("no scope");
        assert!(err.contains("`nv-address` names no scope"), "{err}");
    }

    #[test]
    fn an_unsupported_catalog_version_is_refused() {
        let err =
            ServicesCatalog::parse(&fixture().replace("catalog_version: 2", "catalog_version: 99"))
                .expect_err("unsupported version");
        assert!(err.contains("catalog version 99 is not supported"), "{err}");
    }

    #[test]
    fn an_empty_flat_fee_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace("flat_fee: $50", "flat_fee: ' '"))
            .expect_err("empty flat fee");
        assert!(err.contains("`flat_fee` is empty"), "{err}");
    }

    /// Every category carries a distinct written form, so the enum and its
    /// YAML spelling cannot drift apart.
    #[test]
    fn every_category_has_a_distinct_value() {
        let values: BTreeSet<&str> = ServiceCategory::ALL
            .iter()
            .map(|category| category.as_str())
            .collect();
        assert_eq!(values.len(), ServiceCategory::ALL.len());
        for category in ServiceCategory::ALL {
            let round_trip: ServiceCategory =
                serde_yaml::from_str(category.as_str()).expect("category round-trips");
            assert_eq!(round_trip, *category);
        }
    }

    fn packaged_fixture() -> String {
        fixture().replace(
            "    keywords:\n      - LLC\n",
            "    keywords:\n      - LLC\n    package:\n      of:\n        - nv-address\n      extras:\n        - name: An agreement between the owners\n          amount: $50\n",
        )
    }

    /// A package publishes the Notations it includes in catalog order.
    #[test]
    fn a_package_quote_is_derived_from_member_fees() {
        let catalog = ServicesCatalog::parse(&packaged_fixture()).expect("packaged fixture");
        let llc = catalog.get("llc-file").expect("llc-file");
        let quote = catalog.package_quote(llc).expect("llc-file is a package");
        assert_eq!(
            quote.members,
            vec![
                "Nevada business address".to_string(),
                "An agreement between the owners".to_string()
            ]
        );
    }

    #[test]
    fn a_plan_price_must_be_at_least_half_off_a_notation_package() {
        let catalog = ServicesCatalog::parse(&fixture().replace(
            "    keywords:\n      - LLC\n",
            "    keywords:\n      - LLC\n    package:\n      of:\n        - nv-address\n      extras:\n        - name: An agreement between the owners\n          amount: $50\n    plan_price:\n      amount: $25\n      plan: Business plan\n",
        ))
        .expect("a half-off package plan price");
        assert_eq!(
            catalog
                .get("llc-file")
                .and_then(|service| service.plan_price.as_ref())
                .map(|price| price.amount.as_str()),
            Some("$25")
        );
        let err = ServicesCatalog::parse(&fixture().replace(
            "    keywords:\n      - LLC\n",
            "    keywords:\n      - LLC\n    package:\n      of:\n        - nv-address\n      extras:\n        - name: An agreement between the owners\n          amount: $50\n    plan_price:\n      amount: $26\n      plan: Business plan\n",
        ))
        .expect_err("a plan price above half off");
        assert!(err.contains("plan price is not at least half off"), "{err}");
    }

    #[test]
    fn a_plan_price_requires_a_notation_package() {
        let err = ServicesCatalog::parse(&fixture().replace(
            "    keywords:\n      - LLC\n",
            "    keywords:\n      - LLC\n    plan_price:\n      amount: $25\n      plan: Business plan\n",
        ))
        .expect_err("a plan price on a non-package service");
        assert!(
            err.contains("publishes a plan price but is not a Notation package"),
            "{err}"
        );
    }

    /// The start block is the copy a visitor reads at the service door and on
    /// the matter they just opened — the "you are not a client yet" sentences.
    /// A catalog may omit it entirely (no brand has to publish a door), but a
    /// half-authored one would publish a blank disclosure or a nameless
    /// button, so every field is required once any of them is set.
    #[test]
    fn a_partly_authored_start_block_is_refused() {
        let catalog = ServicesCatalog::parse(&fixture()).expect("no start block is allowed");
        assert_eq!(catalog.start, StartDoorCopy::default());

        let err = ServicesCatalog::parse(&format!(
            "{}\nstart:\n  confirmation: We opened a file.\n  disclosure: Not a client yet.\n  \
             label: Start\n  microcopy: ''\n  refusal: We cannot start this online.\n",
            fixture()
        ))
        .expect_err("a blank field in an authored start block");
        assert!(err.contains("`start.microcopy` is empty"), "{err}");
    }

    /// The whole block round-trips, so the door and the post-start
    /// confirmation read the same strings the catalog publishes.
    #[test]
    fn an_authored_start_block_parses_every_field() {
        let catalog = ServicesCatalog::parse(&format!(
            "{}\nstart:\n  confirmation: We opened a file.\n  disclosure: Not a client yet.\n  \
             label: Start\n  microcopy: Opens a short questionnaire.\n  refusal: We cannot \
             start this online.\n",
            fixture()
        ))
        .expect("a fully authored start block");
        assert_eq!(catalog.start.confirmation, "We opened a file.");
        assert_eq!(catalog.start.disclosure, "Not a client yet.");
        assert_eq!(catalog.start.label, "Start");
        assert_eq!(catalog.start.microcopy, "Opens a short questionnaire.");
        assert_eq!(catalog.start.refusal, "We cannot start this online.");
    }

    #[test]
    fn a_package_of_an_unknown_service_is_refused() {
        let err = ServicesCatalog::parse(
            &packaged_fixture().replace("        - nv-address\n", "        - trust\n"),
        )
        .expect_err("dangling package member");
        assert!(
            err.contains("is a package of `trust`, which this catalog does not define"),
            "{err}"
        );
    }

    #[test]
    fn a_package_of_itself_is_refused() {
        let err = ServicesCatalog::parse(
            &packaged_fixture().replace("        - nv-address\n", "        - llc-file\n"),
        )
        .expect_err("self package");
        assert!(err.contains("lists itself as a package member"), "{err}");
    }

    #[test]
    fn a_package_priced_at_or_above_its_members_is_refused() {
        // llc-file is $50. Drop nv-address to $25 and add a $25 extra: $50
        // bought separately against a $50 package, which is not less.
        let err = ServicesCatalog::parse(
            &fixture()
                .replace(
                    "    keywords:\n      - LLC\n",
                    "    keywords:\n      - LLC\n    package:\n      of:\n        - nv-address\n      extras:\n        - name: A second address\n          amount: $25\n",
                )
                .replace("    amount: $350\n", "    amount: $25\n"),
        )
        .expect_err("package not cheaper");
        assert!(
            err.contains("does not cost less than buying each included Notation on its own"),
            "{err}"
        );
    }

    #[test]
    fn a_package_of_one_notation_is_refused() {
        let err = ServicesCatalog::parse(&fixture().replace(
            "    keywords:\n      - LLC\n",
            "    keywords:\n      - LLC\n    package:\n      of:\n        - nv-address\n",
        ))
        .expect_err("one member");
        assert!(err.contains("is a package of one Notation"), "{err}");
    }

    #[test]
    fn grouped_whole_dollar_fees_parse_as_cents() {
        assert_eq!(fee_cents("$3,000"), Some(300_000));
        assert_eq!(fee_cents("$100"), Some(10_000));
        assert_eq!(fee_cents("$12.50"), Some(1_250));
    }
}
