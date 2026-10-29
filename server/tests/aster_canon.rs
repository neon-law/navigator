//! Asset drift checks, not a perceptual certification of the web translation.
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Canon {
    name: String,
    reference_url: String,
    reference_sha256: String,
    palette: Vec<String>,
    asset: Vec<Asset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    sha256: String,
    visual_status: String,
    required_groups: Vec<String>,
}

fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("public/brand/aster")
}

fn canon() -> Canon {
    toml::from_str(&std::fs::read_to_string(directory().join("CANON.toml")).unwrap()).unwrap()
}

#[test]
fn approved_board_and_palette_remain_the_authority() {
    let canon = canon();
    assert_eq!(canon.name, "Aster the Astrocartographer");
    assert_eq!(
        canon.reference_sha256, "7d37956017b0eab42e899a0ab63e29651b974a2c5dbb4fe2bfb77dc86a96f8bf",
        "Replacing the user's canonical board requires explicit visual approval"
    );
    assert_eq!(
        canon.reference_url,
        "https://github.com/user-attachments/assets/95033122-1aa8-415e-bcb4-11d1709e81cd"
    );
    assert_eq!(
        canon.palette,
        ["#E9B949", "#236B70", "#D9826B", "#182A3A", "#F5EEDF"]
    );
}

#[test]
fn every_character_asset_is_registered_and_unchanged() {
    let canon = canon();
    assert_eq!(
        canon.asset.len(),
        1,
        "Use one master; review new variants against the board"
    );
    assert_eq!(canon.asset[0].path, "aster.svg");
    let mut files: Vec<_> = std::fs::read_dir(directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["CANON.toml", "aster.svg"],
        "Unregistered character asset"
    );
    for asset in canon.asset {
        let bytes = std::fs::read(directory().join(&asset.path)).unwrap();
        let mut checksum = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            write!(&mut checksum, "{byte:02x}").unwrap();
        }
        assert_eq!(checksum, asset.sha256,
            "Aster artwork changed: compare it against the canonical board before updating the checksum");
        // This is intentionally not an approval of the existing simplified redraw.
        assert_eq!(asset.visual_status, "simplified_translation_needs_review",
            "Automated drift checks cannot grant visual approval; record a human comparison before changing this guard");
    }
}

#[test]
fn the_shared_master_keeps_both_hands_both_legs_and_accessories() {
    let canon = canon();
    let asset = &canon.asset[0];
    let mut groups = asset.required_groups.clone();
    groups.sort();
    assert_eq!(
        groups,
        [
            "aster",
            "body",
            "brush",
            "face",
            "left-arm",
            "left-hand",
            "left-leg",
            "right-arm",
            "right-hand",
            "right-leg",
            "satchel",
            "scarf"
        ]
    );
    let svg = std::fs::read_to_string(directory().join(&asset.path)).unwrap();
    for group in groups {
        assert_eq!(
            svg.matches(&format!("id=\"{group}\"")).count(),
            1,
            "Missing or duplicated {group}"
        );
    }
    assert!(svg.contains(&format!("<title>{}</title>", canon.name)));
    assert!(
        !svg.contains("<image"),
        "Keep the master editable rather than embedding a different character"
    );
    assert!(!svg.contains("<script"));
}
