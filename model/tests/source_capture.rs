use model::source_capture::{ExportFilter, Fingerprint, Sections};

const EXPORT: &[u8] = include_bytes!("fixtures/source-capture/v1/export.surql");
const CANONICAL: &[u8] = include_bytes!("fixtures/source-capture/v1/canonical.surql");

#[test]
fn version_one_capture_preserves_record_ids_and_matches_the_canonical_fingerprint() {
    let derived = ["draft".to_owned()];
    let mut sections = Sections::new(derived.clone());
    let buffered: String = std::str::from_utf8(EXPORT)
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| sections.retain(line))
        .collect();
    assert_eq!(buffered.as_bytes(), CANONICAL);

    let mut filter = ExportFilter::new(derived);
    let mut streamed = Fingerprint::default();
    let mut emit = |bytes: &[u8]| streamed.update(bytes).unwrap();
    for chunk in EXPORT.chunks(3) {
        filter.feed(chunk, &mut emit).unwrap();
    }
    filter.finish(&mut emit).unwrap();
    let fingerprint = streamed.finish();
    assert_eq!(
        fingerprint,
        (
            242,
            "a289817bfe2fb5c0d4346f1687519bde75220b0640e3c51c3ce88f1c244059d4".to_owned()
        )
    );
    assert_eq!(Fingerprint::read(CANONICAL).unwrap().finish(), fingerprint);
    assert_ne!(Fingerprint::read(EXPORT).unwrap().finish(), fingerprint);
}
