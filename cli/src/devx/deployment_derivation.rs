//! The coordinates a deployment row never has to spell out.
//!
//! Most of a row is a function of three inputs: the GCP project id, the row
//! name, and the location. `ops gcp setup` names the project's buckets
//! `<project><suffix>`, the KMS key path is fixed by [`gcp::kms::key_name`],
//! and the GKE context is the spelling `gcloud container clusters
//! get-credentials` writes. Storing those values again in every row is how
//! copies drift, so the published record holds only what cannot be derived
//! and every reader fills in the rest here.
//!
//! A coordinate stored in the record always wins: the loaded row is
//! `derived ∪ stored`, with `stored` taking precedence key by key. A derived
//! default is never added under a name the row lists as secret key material,
//! because a name may be a coordinate or a secret, never both.

use std::collections::BTreeMap;

use super::deployments::Deployment;
use super::gcp;

/// The coordinate holding the GCP project id.
pub(super) const PROJECT_ID: &str = "NAVIGATOR_GCP_PROJECT_ID";
/// The coordinate holding the GCP location (region).
pub(super) const LOCATION: &str = "NAVIGATOR_GCP_LOCATION";
const CLUSTER: &str = "NAVIGATOR_GKE_CLUSTER_NAME";
const CONTEXT: &str = "NAVIGATOR_GKE_CONTEXT";

/// Coordinates a reader may not drop: everything else derives from them.
pub(super) const ANCHORS: &[&str] = &[PROJECT_ID, LOCATION];

/// Bucket coordinates and the suffix `ops gcp setup` appends to the project id.
const BUCKETS: &[(&str, &str)] = &[
    ("NAVIGATOR_ASSETS_BUCKET", gcp::ASSETS_BUCKET_SUFFIX),
    ("NAVIGATOR_DOCUMENTS_BUCKET", gcp::DOCUMENTS_BUCKET_SUFFIX),
    ("NAVIGATOR_EXPORTS_BUCKET", gcp::EXPORTS_BUCKET_SUFFIX),
    ("NAVIGATOR_LOGS_BUCKET", gcp::LOGS_BUCKET_SUFFIX),
    (
        "NAVIGATOR_APPLICATIONS_BUCKET",
        gcp::APPLICATIONS_BUCKET_SUFFIX,
    ),
    ("NAVIGATOR_TELEMETRY_BUCKET", gcp::TELEMETRY_BUCKET_SUFFIX),
    ("NAVIGATOR_ARCHIVES_BUCKET", ARCHIVES_BUCKET_SUFFIX),
    ("NAVIGATOR_SURREAL_ARCHIVES_BUCKET", ARCHIVES_BUCKET_SUFFIX),
];

const ARCHIVES_BUCKET_SUFFIX: &str = "-archives";

/// Every coordinate derived from `(project, location)` and the cluster name.
///
/// `cluster` is the effective cluster — a stored override, or the project id —
/// so the derived GKE context always names the cluster the row really uses.
#[must_use]
pub(super) fn derived(project: &str, location: &str, cluster: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = [
        (CLUSTER, project.to_owned()),
        ("NAVIGATOR_K8S_NAMESPACE", project.to_owned()),
        ("NAVIGATOR_VPC_NAME", format!("{project}-vpc")),
        ("NAVIGATOR_SUBNETWORK_NAME", format!("{project}-subnet")),
        ("NAVIGATOR_GATEWAY_IP_NAME", format!("{project}-gateway-ip")),
        ("NAVIGATOR_GCP_SERVICE_ACCOUNT_ID", format!("{project}-web")),
        (
            "NAVIGATOR_DRIVE_GCP_SERVICE_ACCOUNT_ID",
            format!("{project}-drive"),
        ),
        (
            "NAVIGATOR_WEB_SECRET_NAME",
            format!("{project}-web-secrets"),
        ),
        (CONTEXT, format!("gke_{project}_{location}_{cluster}")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    for (key, suffix) in BUCKETS {
        out.insert((*key).to_owned(), format!("{project}{suffix}"));
    }
    out
}

/// The KMS key path for a row, in the spelling `.sops.yaml` carries.
#[must_use]
pub(super) fn kms_key(project: &str, location: &str) -> String {
    gcp::kms::key_name(project, location)
}

/// The row a reader sees: `stored` with every derivable coordinate it does not
/// set filled in. Stored values always win. A row without its project or
/// location coordinate is returned unchanged; validation reports it.
#[must_use]
pub(super) fn effective(stored: &Deployment) -> Deployment {
    let mut row = stored.clone();
    let (Some(project), Some(location)) = (
        stored.coordinates.get(PROJECT_ID).cloned(),
        stored.coordinates.get(LOCATION).cloned(),
    ) else {
        return row;
    };
    let cluster = stored
        .coordinates
        .get(CLUSTER)
        .cloned()
        .unwrap_or_else(|| project.clone());
    let secret_names = |key: &str| {
        stored.encrypted_keys.contains(key)
            || stored.postgres_encrypted_keys.contains(key)
            || stored.forgejo_encrypted_keys.contains(key)
    };
    for (key, value) in derived(&project, &location, &cluster) {
        if !secret_names(&key) {
            row.coordinates.entry(key).or_insert(value);
        }
    }
    if row.kms_key.trim().is_empty() {
        row.kms_key = kms_key(&project, &location);
    }
    row
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn stored(coordinates: &[(&str, &str)]) -> Deployment {
        Deployment {
            name: "example-row".into(),
            kms_key: String::new(),
            provisioned: true,
            coordinates: coordinates
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            encrypted_keys: BTreeSet::new(),
            postgres_encrypted_keys: BTreeSet::new(),
            forgejo_encrypted_keys: BTreeSet::new(),
        }
    }

    #[test]
    fn derives_every_resource_name_from_the_project_and_location() {
        let row = effective(&stored(&[
            (PROJECT_ID, "example-project"),
            (LOCATION, "example-region"),
        ]));
        let get = |key: &str| row.coordinates.get(key).map(String::as_str);
        assert_eq!(get(CLUSTER), Some("example-project"));
        assert_eq!(get("NAVIGATOR_K8S_NAMESPACE"), Some("example-project"));
        assert_eq!(get("NAVIGATOR_VPC_NAME"), Some("example-project-vpc"));
        assert_eq!(
            get("NAVIGATOR_SUBNETWORK_NAME"),
            Some("example-project-subnet")
        );
        assert_eq!(
            get("NAVIGATOR_GATEWAY_IP_NAME"),
            Some("example-project-gateway-ip")
        );
        assert_eq!(
            get("NAVIGATOR_GCP_SERVICE_ACCOUNT_ID"),
            Some("example-project-web")
        );
        assert_eq!(
            get("NAVIGATOR_DRIVE_GCP_SERVICE_ACCOUNT_ID"),
            Some("example-project-drive")
        );
        assert_eq!(
            get("NAVIGATOR_WEB_SECRET_NAME"),
            Some("example-project-web-secrets")
        );
        for (key, bucket) in [
            ("NAVIGATOR_ASSETS_BUCKET", "example-project-assets"),
            ("NAVIGATOR_DOCUMENTS_BUCKET", "example-project-documents"),
            ("NAVIGATOR_EXPORTS_BUCKET", "example-project-exports"),
            ("NAVIGATOR_LOGS_BUCKET", "example-project-logs"),
            (
                "NAVIGATOR_APPLICATIONS_BUCKET",
                "example-project-applications",
            ),
            ("NAVIGATOR_TELEMETRY_BUCKET", "example-project-telemetry"),
            ("NAVIGATOR_ARCHIVES_BUCKET", "example-project-archives"),
            (
                "NAVIGATOR_SURREAL_ARCHIVES_BUCKET",
                "example-project-archives",
            ),
        ] {
            assert_eq!(get(key), Some(bucket), "{key}");
        }
        assert_eq!(
            get(CONTEXT),
            Some("gke_example-project_example-region_example-project")
        );
        assert_eq!(
            row.kms_key,
            "projects/example-project/locations/example-region/keyRings/navigator-secrets/\
             cryptoKeys/deployment-config"
        );
    }

    #[test]
    fn stored_coordinates_override_derived_ones() {
        let mut row = stored(&[
            (PROJECT_ID, "example-project"),
            (LOCATION, "example-region"),
            (CLUSTER, "example-cluster"),
            ("NAVIGATOR_LOGS_BUCKET", "example-logs"),
        ]);
        row.kms_key = "projects/example-project/locations/other/keyRings/r/cryptoKeys/k".into();
        let row = effective(&row);
        assert_eq!(row.coordinates["NAVIGATOR_LOGS_BUCKET"], "example-logs");
        assert_eq!(row.coordinates[CLUSTER], "example-cluster");
        // The derived context follows the effective cluster.
        assert_eq!(
            row.coordinates[CONTEXT],
            "gke_example-project_example-region_example-cluster"
        );
        assert!(row.kms_key.contains("/locations/other/"));
    }

    #[test]
    fn a_secret_key_name_is_never_filled_in_as_a_coordinate() {
        let mut row = stored(&[
            (PROJECT_ID, "example-project"),
            (LOCATION, "example-region"),
        ]);
        row.encrypted_keys
            .insert("NAVIGATOR_WEB_SECRET_NAME".into());
        let row = effective(&row);
        assert!(!row.coordinates.contains_key("NAVIGATOR_WEB_SECRET_NAME"));
    }

    #[test]
    fn a_row_without_its_anchors_is_left_for_validation_to_reject() {
        let row = stored(&[(PROJECT_ID, "example-project")]);
        assert_eq!(effective(&row), row);
    }

    /// The bucket suffixes are `ops gcp setup`'s own, not a second spelling.
    #[test]
    fn bucket_suffixes_are_the_setup_constants() {
        let names: Vec<_> = derived("example-project", "example-region", "example-project")
            .into_iter()
            .filter(|(key, _)| key.ends_with("_BUCKET"))
            .map(|(_, value)| value)
            .collect();
        for suffix in [
            gcp::ASSETS_BUCKET_SUFFIX,
            gcp::DOCUMENTS_BUCKET_SUFFIX,
            gcp::EXPORTS_BUCKET_SUFFIX,
            gcp::LOGS_BUCKET_SUFFIX,
            gcp::APPLICATIONS_BUCKET_SUFFIX,
            gcp::TELEMETRY_BUCKET_SUFFIX,
        ] {
            assert!(
                names.contains(&format!("example-project{suffix}")),
                "{suffix}"
            );
        }
    }
}
