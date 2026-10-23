//! Grow a live PostgreSQL data volume to the size its `StatefulSet` template
//! declares.
//!
//! A `StatefulSet`'s `volumeClaimTemplates` are immutable, so `kubectl apply` of
//! a larger template onto an existing `navigator-postgres` fails. `ops ship`
//! closes that gap before its PostgreSQL apply: it expands each bound claim in
//! place (the `navigator-postgres-retain` `StorageClass` allows expansion), then
//! deletes only the `StatefulSet` object with `--cascade=orphan` so the pod and
//! its claims survive, and the apply that follows recreates the object with the
//! new template. A claim is never shrunk.
//!
//! This module is the pure planner: it reads the rendered and live objects as
//! JSON and returns the actions. `ship.rs` fetches the objects and runs them.

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize as _;
use serde_json::Value;

/// The data claim a rendered `StatefulSet` declares: the template every pod's
/// `PersistentVolumeClaim` is stamped from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaimTemplate {
    pub statefulset: String,
    pub template: String,
    pub replicas: u32,
    pub storage: String,
}

impl ClaimTemplate {
    /// The claim names the `StatefulSet` controller derives:
    /// `<template>-<statefulset>-<ordinal>`.
    pub fn claim_names(&self) -> Vec<String> {
        (0..self.replicas)
            .map(|ordinal| format!("{}-{}-{ordinal}", self.template, self.statefulset))
            .collect()
    }
}

/// One step that converges a live data volume on its template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VolumeAction {
    /// Raise a bound claim's requested storage; the CSI driver grows the disk
    /// and the filesystem online.
    ExpandClaim {
        claim: String,
        from: String,
        to: String,
    },
    /// Delete only the `StatefulSet` object, leaving its pod and claims, so the
    /// apply can recreate it with the changed `volumeClaimTemplates`.
    RecreateOrphaned { statefulset: String },
}

impl VolumeAction {
    /// The `kubectl` arguments after `--context … -n …`.
    pub fn kubectl_args(&self) -> Vec<String> {
        match self {
            Self::ExpandClaim { claim, to, .. } => vec![
                "patch".into(),
                "pvc".into(),
                claim.clone(),
                "--type=merge".into(),
                "-p".into(),
                serde_json::json!({"spec": {"resources": {"requests": {"storage": to}}}})
                    .to_string(),
            ],
            Self::RecreateOrphaned { statefulset } => vec![
                "delete".into(),
                "statefulset".into(),
                statefulset.clone(),
                "--cascade=orphan".into(),
                "--wait=true".into(),
            ],
        }
    }

    /// One plan line for the operator.
    pub fn describe(&self) -> String {
        match self {
            Self::ExpandClaim { claim, from, to } => {
                format!("expand pvc/{claim} {from} → {to}")
            }
            Self::RecreateOrphaned { statefulset } => format!(
                "recreate statefulset/{statefulset} (--cascade=orphan; pod and claims kept)"
            ),
        }
    }
}

/// Read the named `StatefulSet`'s first claim template from a rendered
/// multi-document manifest.
pub(super) fn desired_claim(rendered: &str, statefulset: &str) -> Result<ClaimTemplate> {
    for document in serde_yaml::Deserializer::from_str(rendered) {
        let Ok(value) = Value::deserialize(document) else {
            continue;
        };
        if value["kind"].as_str() != Some("StatefulSet")
            || value.pointer("/metadata/name").and_then(Value::as_str) != Some(statefulset)
        {
            continue;
        }
        let claim = value
            .pointer("/spec/volumeClaimTemplates/0")
            .with_context(|| format!("statefulset/{statefulset} declares no claim template"))?;
        let template = claim
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .with_context(|| format!("statefulset/{statefulset} claim template has no name"))?;
        let storage = template_storage(&value)
            .with_context(|| format!("statefulset/{statefulset} claim template has no size"))?;
        let replicas = value
            .pointer("/spec/replicas")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        return Ok(ClaimTemplate {
            statefulset: statefulset.to_owned(),
            template: template.to_owned(),
            replicas: u32::try_from(replicas).context("replica count fits in u32")?,
            storage: storage.to_owned(),
        });
    }
    bail!("statefulset/{statefulset} is missing from the rendered manifest")
}

/// The first claim template's requested storage on a `StatefulSet` object.
pub(super) fn template_storage(statefulset: &Value) -> Option<&str> {
    statefulset
        .pointer("/spec/volumeClaimTemplates/0/spec/resources/requests/storage")
        .and_then(Value::as_str)
}

/// A claim's requested storage.
pub(super) fn claim_storage(claim: &Value) -> Option<&str> {
    claim
        .pointer("/spec/resources/requests/storage")
        .and_then(Value::as_str)
}

/// Plan the actions that bring a live `StatefulSet`'s data volume to `desired`.
///
/// `live_template` is the live `StatefulSet`'s template size (`None` when the
/// `StatefulSet` does not exist yet: the apply creates it at the desired size).
/// `claims` pairs each derived claim name with its live requested size
/// (`None` when that claim does not exist yet).
pub(super) fn plan(
    desired: &ClaimTemplate,
    live_template: Option<&str>,
    claims: &[(String, Option<String>)],
) -> Result<Vec<VolumeAction>> {
    let Some(live_template) = live_template else {
        return Ok(Vec::new());
    };
    let want = parse_quantity(&desired.storage)?;
    let have = parse_quantity(live_template)?;
    ensure!(
        have <= want,
        "statefulset/{} declares {live_template} of data but the release renders {}; \
         a PostgreSQL data volume is never shrunk",
        desired.statefulset,
        desired.storage
    );
    let mut actions = Vec::new();
    for (claim, size) in claims {
        let Some(size) = size else { continue };
        if parse_quantity(size)? < want {
            actions.push(VolumeAction::ExpandClaim {
                claim: claim.clone(),
                from: size.clone(),
                to: desired.storage.clone(),
            });
        }
    }
    if have < want {
        actions.push(VolumeAction::RecreateOrphaned {
            statefulset: desired.statefulset.clone(),
        });
    }
    Ok(actions)
}

/// Parse a Kubernetes integer quantity (`100Gi`, `500M`, `1073741824`) into
/// bytes. Fractional quantities are refused rather than rounded.
pub(super) fn parse_quantity(quantity: &str) -> Result<u128> {
    const SUFFIXES: &[(&str, u128)] = &[
        ("Ki", 1 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("Pi", 1 << 50),
        ("Ei", 1 << 60),
        ("k", 1_000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
        ("P", 1_000_000_000_000_000),
        ("E", 1_000_000_000_000_000_000),
    ];
    let quantity = quantity.trim();
    let (digits, multiplier) = SUFFIXES
        .iter()
        .find_map(|(suffix, multiplier)| {
            quantity
                .strip_suffix(suffix)
                .map(|digits| (digits, *multiplier))
        })
        .unwrap_or((quantity, 1));
    let value: u128 = digits
        .parse()
        .with_context(|| format!("unsupported storage quantity `{quantity}`"))?;
    value
        .checked_mul(multiplier)
        .with_context(|| format!("storage quantity `{quantity}` overflows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postgres(storage: &str) -> ClaimTemplate {
        ClaimTemplate {
            statefulset: "navigator-postgres".into(),
            template: "data".into(),
            replicas: 1,
            storage: storage.into(),
        }
    }

    fn claim(size: &str) -> Vec<(String, Option<String>)> {
        vec![("data-navigator-postgres-0".into(), Some(size.into()))]
    }

    #[test]
    fn claim_names_follow_the_statefulset_controller() {
        let mut template = postgres("100Gi");
        assert_eq!(template.claim_names(), ["data-navigator-postgres-0"]);
        template.replicas = 2;
        assert_eq!(
            template.claim_names(),
            ["data-navigator-postgres-0", "data-navigator-postgres-1"]
        );
    }

    #[test]
    fn a_first_release_has_nothing_to_grow() {
        assert_eq!(plan(&postgres("100Gi"), None, &[]).unwrap(), []);
    }

    #[test]
    fn a_converged_row_has_nothing_to_do() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("100Gi"), &claim("100Gi")).unwrap(),
            []
        );
    }

    #[test]
    fn a_smaller_row_expands_its_claim_then_recreates_the_statefulset_orphaned() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("20Gi"), &claim("20Gi")).unwrap(),
            [
                VolumeAction::ExpandClaim {
                    claim: "data-navigator-postgres-0".into(),
                    from: "20Gi".into(),
                    to: "100Gi".into(),
                },
                VolumeAction::RecreateOrphaned {
                    statefulset: "navigator-postgres".into(),
                },
            ]
        );
    }

    #[test]
    fn a_claim_already_expanded_by_an_interrupted_release_only_recreates() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("20Gi"), &claim("100Gi")).unwrap(),
            [VolumeAction::RecreateOrphaned {
                statefulset: "navigator-postgres".into(),
            }]
        );
    }

    #[test]
    fn a_claim_left_small_under_a_grown_template_is_expanded_without_recreating() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("100Gi"), &claim("20Gi")).unwrap(),
            [VolumeAction::ExpandClaim {
                claim: "data-navigator-postgres-0".into(),
                from: "20Gi".into(),
                to: "100Gi".into(),
            }]
        );
    }

    #[test]
    fn a_missing_claim_is_left_for_the_controller_to_create() {
        let claims = vec![("data-navigator-postgres-0".into(), None)];
        assert_eq!(
            plan(&postgres("100Gi"), Some("20Gi"), &claims).unwrap(),
            [VolumeAction::RecreateOrphaned {
                statefulset: "navigator-postgres".into(),
            }]
        );
    }

    #[test]
    fn a_larger_claim_is_never_shrunk() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("100Gi"), &claim("200Gi")).unwrap(),
            []
        );
        let error = plan(&postgres("100Gi"), Some("200Gi"), &claim("200Gi")).unwrap_err();
        assert!(error.to_string().contains("never shrunk"), "{error}");
    }

    #[test]
    fn equal_quantities_in_different_units_are_converged() {
        assert_eq!(
            plan(&postgres("100Gi"), Some("102400Mi"), &claim("107374182400")).unwrap(),
            []
        );
    }

    #[test]
    fn quantities_parse_binary_and_decimal_suffixes() {
        assert_eq!(parse_quantity("20Gi").unwrap(), 20 << 30);
        assert_eq!(parse_quantity("100G").unwrap(), 100_000_000_000);
        assert_eq!(parse_quantity("512").unwrap(), 512);
        assert!(parse_quantity("1.5Gi").is_err());
        assert!(parse_quantity("lots").is_err());
    }

    #[test]
    fn actions_render_the_exact_kubectl_calls() {
        let expand = VolumeAction::ExpandClaim {
            claim: "data-navigator-postgres-0".into(),
            from: "20Gi".into(),
            to: "100Gi".into(),
        };
        assert_eq!(
            expand.kubectl_args(),
            [
                "patch",
                "pvc",
                "data-navigator-postgres-0",
                "--type=merge",
                "-p",
                r#"{"spec":{"resources":{"requests":{"storage":"100Gi"}}}}"#,
            ]
        );
        assert_eq!(
            expand.describe(),
            "expand pvc/data-navigator-postgres-0 20Gi → 100Gi"
        );
        let recreate = VolumeAction::RecreateOrphaned {
            statefulset: "navigator-postgres".into(),
        };
        assert_eq!(
            recreate.kubectl_args(),
            [
                "delete",
                "statefulset",
                "navigator-postgres",
                "--cascade=orphan",
                "--wait=true",
            ]
        );
    }

    #[test]
    fn the_desired_claim_comes_from_the_rendered_statefulset() {
        let rendered = "\
apiVersion: v1
kind: Service
metadata:
  name: navigator-postgres
---
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: navigator-postgres
spec:
  replicas: 1
  volumeClaimTemplates:
  - metadata:
      name: data
    spec:
      resources:
        requests:
          storage: 100Gi
";
        assert_eq!(
            desired_claim(rendered, "navigator-postgres").unwrap(),
            postgres("100Gi")
        );
        assert!(desired_claim(rendered, "navigator-forgejo").is_err());
    }

    #[test]
    fn the_base_manifest_declares_one_hundred_gibibytes() {
        let base = include_str!("../../../k8s/postgres/postgres.yaml");
        let claim = desired_claim(base, "navigator-postgres").unwrap();
        assert_eq!(claim.storage, "100Gi");
        assert_eq!(claim.claim_names(), ["data-navigator-postgres-0"]);
    }
}
