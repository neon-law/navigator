//! Pin the approved original pixels; the website frames them without redrawing Aster.
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
    asset_key: String,
    width: u32,
    height: u32,
    palette: Vec<String>,
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
    assert_eq!(canon.asset_key, views::assets::ASTER_BOARD_KEY);
    assert_eq!((canon.width, canon.height), (1774, 887));
    assert_eq!(
        canon.palette,
        ["#E9B949", "#236B70", "#D9826B", "#182A3A", "#F5EEDF"]
    );
}

#[test]
fn there_are_no_separately_redrawn_character_assets() {
    let files: Vec<_> = std::fs::read_dir(directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        files,
        ["CANON.toml"],
        "Use the approved board rather than an unreviewed redraw"
    );
}

/// Public raster assets stay outside Git. Run this after restoring or publishing the original asset.
#[test]
#[ignore = "requires the approved PNG restored through the public asset lane"]
fn restored_board_is_byte_for_byte_the_user_approved_attachment() {
    let canon = canon();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("public")
        .join(&canon.asset_key);
    let bytes =
        std::fs::read(path).expect("restore the original canonical board before verifying it");
    let mut checksum = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        write!(&mut checksum, "{byte:02x}").unwrap();
    }
    assert_eq!(
        checksum, canon.reference_sha256,
        "Aster's original eyes, texture and artwork must remain unchanged"
    );
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (canon.width, canon.height));
}
