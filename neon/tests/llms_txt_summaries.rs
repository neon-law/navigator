//! Every live house brand's `/llms.txt` must give a crawler more than its own
//! name: a plain-text understanding of what the practice actually does,
//! not just a title it can already read from the `Host:` header.

use views::brand::BrandKey;

#[test]
fn every_live_brands_llms_txt_summarizes_what_the_practice_does() {
    for key in BrandKey::LIVE {
        let document = neon::llms_for(&[], *key);
        let mark = key
            .resolve_branding(&views::brand::DEFAULT_BRANDING)
            .firm
            .site_name;

        let summary = document.summary.trim();
        assert!(
            !summary.is_empty(),
            "{} publishes no llms.txt summary",
            key.as_str()
        );
        assert_ne!(
            summary,
            mark,
            "{} llms.txt summary is just its own name, not a description of what it does",
            key.as_str()
        );
        assert!(
            summary.split_whitespace().count() >= 8,
            "{} llms.txt summary is too short to explain what the practice does: {summary:?}",
            key.as_str()
        );

        let home = document
            .pages
            .first()
            .unwrap_or_else(|| panic!("{} llms.txt has no pages", key.as_str()));
        assert!(
            !home.description.trim().is_empty(),
            "{} llms.txt home entry has no description",
            key.as_str()
        );
    }
}

fn material(category: &str, slug: &str, sections: usize) -> portal::WorkshopMaterial {
    portal::WorkshopMaterial {
        category: category.into(),
        slug: slug.into(),
        title: format!("Synthetic {slug}"),
        description: "A synthetic catalog entry.".into(),
        audience: String::new(),
        benefit: String::new(),
        raw_markdown: String::new(),
        body_html: String::new(),
        intro_html: String::new(),
        chapters: Vec::new(),
        sections: (0..sections)
            .map(|_| portal::workshops::WorkshopSection {
                title: String::new(),
                body_html: String::new(),
                notes_html: String::new(),
            })
            .collect(),
    }
}

/// Whether the site declares `path`, reading `{name}` as one segment.
fn declared(path: &str) -> bool {
    neon::brand().public_paths.iter().any(|pattern| {
        let pattern: Vec<_> = pattern.split('/').collect();
        let path: Vec<_> = path.split('/').collect();
        pattern.len() == path.len()
            && pattern
                .iter()
                .zip(&path)
                .all(|(expected, actual)| expected.starts_with('{') || expected == actual)
    })
}

#[test]
fn every_brand_sends_a_crawler_only_to_pages_it_publishes() {
    let catalog = [material("presentations", "synthetic-talk", 2)];
    for key in BrandKey::ALL {
        let sitemap = neon::sitemap_for(&catalog, *key);
        assert!(sitemap.contains("/"), "{}", key.as_str());
        for path in sitemap.iter().filter(|path| !path.contains('.')) {
            assert!(declared(path), "{} lists undeclared {path}", key.as_str());
            assert!(
                key.publishes_firm_path(path),
                "{} lists {path}, which it does not publish",
                key.as_str()
            );
        }
    }
}

#[test]
fn the_firm_sitemap_expands_each_talk_into_its_readable_faces_only() {
    let catalog = [
        material("presentations", "synthetic-talk", 2),
        material("workshops", "synthetic-workshop", 0),
        material("drafts", "unlisted", 3),
    ];
    let sitemap = neon::sitemap_for(&catalog, BrandKey::Neon);
    for path in [
        "/presentations/synthetic-talk",
        "/presentations/synthetic-talk.md",
        "/presentations/synthetic-talk/slides",
        "/presentations/synthetic-talk/step/1",
        "/presentations/synthetic-talk/step/2",
        "/workshops/synthetic-workshop",
    ] {
        assert!(sitemap.contains(path), "{path}");
    }
    // A projector face, a step past the last section, a redirect, an
    // uncatalogued category, and a route pattern are not crawlable documents.
    assert!(!sitemap.contains("/presentations/synthetic-talk/step/3"));
    assert!(!sitemap.contains("/workshops/synthetic-workshop/step/1"));
    for path in &sitemap {
        assert!(!path.contains('{') && !path.contains("/display/"), "{path}");
        assert!(!path.starts_with("/drafts"), "{path}");
    }
    for redirect in ["/personal", "/business", "/navigator", "/workshops"] {
        assert!(!sitemap.contains(redirect), "{redirect}");
    }
    // No other brand publishes the firm's catalog.
    assert!(BrandKey::ALL
        .iter()
        .filter(|key| **key != BrandKey::Neon)
        .all(|key| !neon::sitemap_for(&catalog, *key)
            .iter()
            .any(|path| path.contains("synthetic"))));
}

#[test]
fn the_firm_index_lists_the_workshop_corpus_as_markdown() {
    let catalog = [
        material("presentations", "synthetic-talk", 2),
        material("workshops", "synthetic-workshop", 0),
    ];
    let index = neon::llms_for(&catalog, BrandKey::Neon);
    let [corpus] = &index.sections[..] else {
        panic!("one corpus section, got {}", index.sections.len());
    };
    let [link] = &corpus.links[..] else {
        panic!("only the workshop is in the corpus: {}", corpus.links.len());
    };
    assert_eq!(link.path, "/workshops/synthetic-workshop.md");
    assert_eq!(link.title, "Synthetic synthetic-workshop");
    assert!(neon::llms_for(&catalog, BrandKey::Vesta)
        .sections
        .is_empty());
}

#[test]
fn every_indexed_page_is_one_the_brand_publishes() {
    for key in BrandKey::ALL {
        let index = neon::llms_for(&[], *key);
        assert_eq!(index.pages[0].path, "/", "{}", key.as_str());
        for page in &index.pages {
            assert!(
                declared(&page.path) && key.publishes_firm_path(&page.path),
                "{} indexes {}, which it does not publish",
                key.as_str(),
                page.path
            );
            assert!(!page.description.trim().is_empty(), "{}", page.path);
        }
    }
    let paths = |key| -> Vec<String> {
        neon::llms_for(&[], key)
            .pages
            .into_iter()
            .map(|page| page.path)
            .collect()
    };
    assert!(paths(BrandKey::GuiebShook).contains(&"/team".to_owned()));
    assert!(
        paths(BrandKey::SameDayLegal).contains(&views::brand::SAME_DAY_LEGAL_APP_PATH.to_owned())
    );
    assert!(paths(BrandKey::DeleteYourData).contains(&"/contact".to_owned()));
    assert!(!paths(BrandKey::Vesta).contains(&"/team".to_owned()));
}
