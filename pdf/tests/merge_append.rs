//! Appending one PDF's pages to another's is the `AcroForm` packet's
//! composition step: the state's own form, then the rendered template
//! body whose signature anchors the client signs over. The combined
//! packet must carry both sources' text layers intact.

#[test]
fn an_addendum_follows_the_base_pages() {
    let base = pdf::render("Base form page.").expect("render base");
    let addendum = pdf::render("Authorization addendum.").expect("render addendum");

    let combined = pdf::append_document(&base, &addendum).expect("append");

    assert_eq!(pdf::page_count(&combined).expect("page count"), 2);
    assert_eq!(
        pdf::passage::occurrence_count(&combined, "Base form page").expect("base text"),
        1,
        "the base page keeps its text layer",
    );
    assert_eq!(
        pdf::passage::occurrence_count(&combined, "Authorization addendum").expect("addendum text"),
        1,
        "the addendum's text layer survives the copy",
    );
}

#[test]
fn the_addendum_never_shuffles_the_base_pages() {
    let base = pdf::render("Base form page.").expect("render base");
    let addendum = pdf::render("Authorization addendum.").expect("render addendum");

    let combined = pdf::append_document(&base, &addendum).expect("first append");
    let three = pdf::append_document(&combined, &addendum).expect("second append");

    assert_eq!(pdf::page_count(&three).expect("page count"), 3);
    // The lifted addendum must locate as real positioned text, not a
    // flattened raster — the signature fields anchor into it.
    let found = pdf::locate(&three, "Authorization addendum", 2).expect("locate second addendum");
    assert_eq!(found.occurrences, 2, "both addendum copies are findable");
}
