//! The site adapters registered with the scraper framework. One module per
//! portal; each implements the [`SiteAdapter`](crate::scraper::adapter::SiteAdapter)
//! contract and registers in [`crate::scraper::registered_adapters`].

pub(crate) mod la_superior;

pub(crate) use la_superior::LaSuperiorAdapter;
