//! Repository-confined letter selection and a staged letter/manifest migration.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use regex::{Captures, Regex};
use serde_yaml::Value;

use super::manifest;

struct Manifest {
    path: PathBuf,
    contents: String,
    value: Value,
}

fn read_manifest(root: &Path) -> Result<Option<Manifest>> {
    let path = root.join(manifest::FILE);
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read the Project manifest"),
    };
    if !metadata.is_file() {
        bail!("navigator.yaml must be a regular file");
    }
    let contents = std::fs::read_to_string(&path).context("read navigator.yaml")?;
    let parsed = manifest::parse(&contents).map_err(anyhow::Error::msg)?;
    if !rules::pages::shape_problems(&contents).is_empty() {
        bail!("navigator.yaml has an invalid pages declaration; run navigator project gate");
    }
    for page in &parsed.pages {
        if let Some(draft) = &page.draft {
            relative(draft)?;
        }
    }
    let value: Value = serde_yaml::from_str(&contents).context("parse navigator.yaml")?;
    Ok(Some(Manifest {
        path,
        contents,
        value,
    }))
}

fn relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    let mut parts = path.components();
    let admitted = matches!(parts.next(), Some(Component::Normal(name)) if name == "drafts" || name == "templates")
        && parts.clone().next().is_some()
        && parts.all(|part| matches!(part, Component::Normal(_)))
        && path.extension().is_some_and(|extension| extension == "md")
        && !value.contains('\\');
    if !admitted {
        bail!("page draft must be a relative Markdown path under drafts/ or templates/");
    }
    Ok(path)
}

pub(super) fn checked_path(root: &Path, value: &str) -> Result<PathBuf> {
    let path = root.join(relative(value)?);
    if !path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file())
    {
        bail!("the declared letter must be an existing regular file");
    }
    let canonical_root = root
        .canonicalize()
        .context("resolve the Project repository")?;
    let canonical_path = path.canonicalize().context("resolve the declared letter")?;
    if !canonical_path.starts_with(canonical_root.join("drafts"))
        && !canonical_path.starts_with(canonical_root.join("templates"))
    {
        bail!("the declared letter must remain inside the Project repository");
    }
    Ok(path)
}

pub(super) fn declared(root: &Path) -> Result<Option<String>> {
    if root.join("drafts").is_dir() && root.join("templates").is_dir() {
        bail!("the Project has both drafts/ and templates/; choose one Draft directory first");
    }
    let Some(manifest) = read_manifest(root)? else {
        return Ok(None);
    };
    let mut drafts = Vec::new();
    for page in manifest
        .value
        .get("pages")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        if page.get("kind").and_then(Value::as_str) != Some("engagement_billing_records") {
            continue;
        }
        if let Some(draft) = page.get("draft").and_then(Value::as_str) {
            checked_path(root, draft)?;
            drafts.push(draft.to_string());
        }
    }
    drafts.sort();
    drafts.dedup();
    match drafts.as_slice() {
        [] => Ok(None),
        [draft] => Ok(Some(draft.clone())),
        _ => bail!("multiple engagement Drafts are declared; select one before opening or migrating a letter"),
    }
}

fn rewritten_manifest(
    root: &Path,
    source: &Path,
    target: &Path,
) -> Result<Option<(Manifest, String)>> {
    let Some(mut manifest) = read_manifest(root)? else {
        return Ok(None);
    };
    let source = source
        .strip_prefix(root)
        .context("letter source is outside the Project")?;
    let target = target
        .strip_prefix(root)
        .context("letter target is outside the Project")?;
    let target = target
        .to_str()
        .ok_or_else(|| anyhow!("letter target is not UTF-8"))?;
    let mut changed = false;
    if let Some(pages) = manifest
        .value
        .get_mut("pages")
        .and_then(Value::as_sequence_mut)
    {
        for page in pages {
            let Some(draft) = page.get_mut("draft") else {
                continue;
            };
            if draft
                .as_str()
                .is_some_and(|value| Path::new(value) == source)
            {
                *draft = Value::String(target.to_string());
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    // Replace scalar tokens only, then prove the complete YAML value is exactly
    // the planned document. Unsupported encodings refuse before any file changes.
    let scalars = Regex::new(
        r#"(?m)(?P<prefix>(?:^|[ \t{,])(?:draft|"draft"|'draft')[ \t]*:[ \t]*)(?P<value>"(?:\\.|[^"\\])*"|'(?:''|[^'])*'|[^,\r\n}\]#]+)"#,
    )?;
    let updated = scalars
        .replace_all(&manifest.contents, |capture: &Captures<'_>| {
            let raw = &capture["value"];
            let scalar = raw.trim_end();
            if serde_yaml::from_str::<String>(scalar).is_ok_and(|value| Path::new(&value) == source)
            {
                let quoted = match scalar.as_bytes().first() {
                    Some(b'\'') => format!("'{target}'"),
                    Some(b'"') => format!("\"{target}\""),
                    _ => target.to_string(),
                };
                format!("{}{quoted}{}", &capture["prefix"], &raw[scalar.len()..])
            } else {
                capture[0].to_string()
            }
        })
        .into_owned();
    let actual: Value =
        serde_yaml::from_str(&updated).context("validate updated page references")?;
    if actual != manifest.value {
        bail!("cannot safely rewrite the page references; use plain or quoted draft path scalars");
    }
    Ok(Some((manifest, updated)))
}

fn stage(path: &Path, contents: &[u8]) -> Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("letter destination has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("stage the migration")?;
    temporary
        .write_all(contents)
        .context("write staged migration")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync staged migration")?;
    Ok(temporary)
}

fn persist(temporary: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    temporary
        .persist(path)
        .map_err(|error| anyhow!("replace migration file: {}", error.error))?;
    Ok(())
}

fn restore(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(contents) => persist(stage(path, contents)?, path),
        None => std::fs::remove_file(path).context("remove the new migration output"),
    }
}

pub(super) fn migrate(
    root: &Path,
    source: &Path,
    target: &Path,
    original: &str,
    contents: &str,
) -> Result<()> {
    apply(root, source, target, original, contents, |path| {
        std::fs::remove_file(path)
    })
}

fn apply(
    root: &Path,
    source: &Path,
    target: &Path,
    original: &str,
    contents: &str,
    remove_source: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let relative_source = source
        .strip_prefix(root)?
        .to_str()
        .ok_or_else(|| anyhow!("letter source is not UTF-8"))?;
    checked_path(root, relative_source)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("letter destination has no parent"))?;
    let canonical_root = root.canonicalize()?;
    if !parent.canonicalize()?.starts_with(&canonical_root) {
        bail!("letter destination is outside the Project repository");
    }
    let old_target = match target.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => Some(std::fs::read(target)?),
        Ok(_) => bail!("letter destination must be a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let manifest = rewritten_manifest(root, source, target)?;
    let output = stage(target, contents.as_bytes())?;
    let staged_manifest = manifest
        .as_ref()
        .map(|(manifest, updated)| stage(&manifest.path, updated.as_bytes()))
        .transpose()?;
    if std::fs::read(source)? != original.as_bytes()
        || target.try_exists()? != old_target.is_some()
        || old_target.as_ref().is_some_and(|previous| {
            !std::fs::read(target).is_ok_and(|current| &current == previous)
        })
        || manifest.as_ref().is_some_and(|(manifest, _)| {
            !std::fs::read(&manifest.path)
                .is_ok_and(|current| current == manifest.contents.as_bytes())
        })
    {
        bail!("a migration input changed while staging; review the files and retry");
    }
    persist(output, target)?;
    if let Some((manifest, _)) = &manifest {
        let temporary = staged_manifest.ok_or_else(|| anyhow!("manifest staging is missing"))?;
        if let Err(error) = persist(temporary, &manifest.path) {
            restore(target, old_target.as_deref())
                .context("restore the letter after manifest replacement failed")?;
            return Err(error);
        }
    }
    if source != target {
        if let Err(error) = remove_source(source) {
            if let Some((manifest, _)) = &manifest {
                restore(&manifest.path, Some(manifest.contents.as_bytes()))
                    .context("restore manifest after source removal failed")?;
            }
            restore(target, old_target.as_deref())
                .context("restore destination after source removal failed")?;
            return Err(error).context("remove the migrated source letter");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn a_stale_source_snapshot_refuses_before_replacing_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        let source = root.path().join("drafts/old.md");
        let target = root.path().join("drafts/onboarding__letter.md");
        std::fs::write(&source, "New attorney edit.").unwrap();
        std::fs::write(&target, "Original stub.").unwrap();
        let result = apply(
            root.path(),
            &source,
            &target,
            "Stale snapshot.",
            "Migrated letter.",
            |_| panic!("a changed source must never be removed"),
        );
        assert!(result.unwrap_err().to_string().contains("input changed"));
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "New attorney edit."
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "Original stub.");
    }

    #[test]
    fn failed_source_removal_restores_the_manifest_and_destination() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        let source = root.path().join("drafts/old.md");
        let target = root.path().join("drafts/onboarding__letter.md");
        std::fs::write(&source, "Original letter.").unwrap();
        std::fs::write(&target, "Original stub.").unwrap();
        let manifest = "pages:\n  - slug: engagement\n    kind: engagement_billing_records\n    title: Engagement\n    draft: drafts/old.md\n";
        std::fs::write(root.path().join("navigator.yaml"), manifest).unwrap();
        let reached_removal = std::cell::Cell::new(false);
        let result = apply(
            root.path(),
            &source,
            &target,
            "Original letter.",
            "Migrated letter.",
            |_| {
                reached_removal.set(true);
                assert_eq!(
                    std::fs::read_to_string(&target).unwrap(),
                    "Migrated letter."
                );
                assert!(std::fs::read_to_string(root.path().join("navigator.yaml"))
                    .unwrap()
                    .contains("drafts/onboarding__letter.md"));
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            },
        );
        assert!(
            reached_removal.get(),
            "the injected failure occurs after both replacements"
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "Original letter."
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "Original stub.");
        assert_eq!(
            std::fs::read_to_string(root.path().join("navigator.yaml")).unwrap(),
            manifest
        );
    }
}
