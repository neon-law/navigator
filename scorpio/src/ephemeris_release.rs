//! Atomic promotion of verified immutable local reference bundles.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use rustix::fs::{self, AtFlags, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};

use crate::ephemeris_tables::{self, digest, VerifiedBundle, MAX_MANIFEST_BYTES};

const LATEST: &str = "latest-validated.json";
const POINTER_LIMIT: usize = 4096;

#[derive(Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseManifest {
    schema_version: u32,
    release_id: String,
    bundle: String,
    reference: VerifiedBundle,
}

impl ReleaseManifest {
    fn new(bundle: String, reference: VerifiedBundle) -> Result<Self> {
        let mut release = Self {
            schema_version: 1,
            release_id: String::new(),
            bundle,
            reference,
        };
        release.release_id = release.identity()?;
        Ok(release)
    }

    fn identity(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(&(
            "navigator-ephemeris-release-v1",
            &self.bundle,
            &self.reference,
        ))?))
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        let release: Self =
            serde_json::from_slice(bytes).context("invalid latest-validated manifest")?;
        let reference = &release.reference;
        ensure!(
            release.schema_version == 1
                && valid_bundle(&release.bundle)
                && valid_digest(&reference.snapshot_id)
                && valid_digest(&reference.derived_sha256)
                && valid_digest(&reference.source_sha256)
                && reference.iceberg_sha256.as_deref().is_none_or(valid_digest)
                && reference.duckdb_sha256.as_deref().is_none_or(valid_digest)
                && ephemeris_tables::valid_coverage(reference.from, reference.to_exclusive)
                && release.release_id == release.identity()?,
            "latest-validated manifest schema or identity differs"
        );
        Ok(release)
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_bundle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn open_dir(base: impl std::os::fd::AsFd, path: impl rustix::path::Arg) -> Result<File> {
    Ok(File::from(fs::openat(
        base,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

fn ensure_private_root(root: &File) -> Result<()> {
    ensure!(
        root.metadata()?.mode() & 0o022 == 0,
        "local release directory must not be group or world writable"
    );
    Ok(())
}

fn same_directory(first: &File, second: &File) -> Result<bool> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok(first.dev() == second.dev() && first.ino() == second.ino())
}

fn read_at(dir: &File, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    let file = match fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "release entry is not an ordinary file"
    );
    let limit = u64::try_from(limit)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= limit,
        "release entry exceeds its byte bound"
    );
    Ok(Some(bytes))
}

struct LockedRoot(File);

impl LockedRoot {
    fn acquire(root: File) -> Result<Self> {
        match fs::flock(&root, FlockOperation::NonBlockingLockExclusive) {
            Err(Errno::WOULDBLOCK) => bail!("local release directory is busy"),
            result => result.context("lock local release directory")?,
        }
        Ok(Self(root))
    }
}

impl Drop for LockedRoot {
    fn drop(&mut self) {
        // Release the lock at the operation boundary even if a duplicate fd is still open.
        let _ = fs::flock(&self.0, FlockOperation::Unlock);
    }
}

pub(super) struct Promotion {
    root_path: PathBuf,
    root: LockedRoot,
    bundle: String,
    target: File,
    previous_bytes: Option<Vec<u8>>,
    previous: Option<ReleaseManifest>,
    expected: String,
}

impl Promotion {
    pub(super) fn begin(root_path: &Path, bundle: String, expected: String) -> Result<Self> {
        ensure!(
            valid_bundle(&bundle),
            "bundle must be a single ASCII directory name"
        );
        ensure!(
            expected == "absent" || valid_digest(&expected),
            "invalid expected release ID"
        );

        let root_path: PathBuf = root_path.components().collect();
        let root = open_dir(fs::CWD, &root_path).context("open local release directory")?;
        ensure_private_root(&root)?;
        let root = LockedRoot::acquire(root)?;
        let root_path = root_path.canonicalize()?;
        ensure!(
            same_directory(&root.0, &open_dir(fs::CWD, &root_path)?)?,
            "local release directory changed"
        );
        let previous_bytes = read_at(&root.0, LATEST, POINTER_LIMIT)?;
        let previous = previous_bytes
            .as_deref()
            .map(ReleaseManifest::parse)
            .transpose()?;
        let target =
            open_dir(&root.0, bundle.as_str()).context("open existing reference bundle")?;
        Ok(Self {
            root_path,
            root,
            bundle,
            target,
            previous_bytes,
            previous,
            expected,
        })
    }

    pub(super) fn bundle_path(&self) -> PathBuf {
        self.root_path.join(&self.bundle)
    }

    fn recheck(&self, reference: &VerifiedBundle) -> Result<()> {
        ensure_private_root(&self.root.0)?;
        ensure!(
            same_directory(&self.root.0, &open_dir(fs::CWD, &self.root_path)?)?
                && same_directory(&self.target, &open_dir(&self.root.0, self.bundle.as_str())?)?,
            "local release directory or bundle changed"
        );
        ensure!(
            read_at(&self.root.0, LATEST, POINTER_LIMIT)? == self.previous_bytes,
            "latest-validated manifest changed"
        );
        for (name, expected) in [
            (
                ephemeris_tables::MANIFEST,
                Some(reference.derived_sha256.as_str()),
            ),
            (
                ephemeris_tables::SOURCE,
                Some(reference.source_sha256.as_str()),
            ),
            (
                crate::ephemeris_iceberg::RECEIPT,
                reference.iceberg_sha256.as_deref(),
            ),
            (
                crate::ephemeris_duckdb::RECEIPT,
                reference.duckdb_sha256.as_deref(),
            ),
        ] {
            let bytes = read_at(&self.target, name, MAX_MANIFEST_BYTES)?;
            ensure!(
                bytes.as_deref().map(digest).as_deref() == expected,
                "verified bundle receipt changed"
            );
        }
        Ok(())
    }

    pub(super) fn commit(self, reference: VerifiedBundle) -> Result<()> {
        let release = ReleaseManifest::new(self.bundle.clone(), reference)?;
        if self.previous.as_ref() == Some(&release) {
            return self.recheck(&release.reference);
        }
        let current = self
            .previous
            .as_ref()
            .map_or("absent", |prior| prior.release_id.as_str());
        ensure!(current == self.expected, "expected current release differs");

        let bytes = serde_json::to_vec_pretty(&release)?;
        ensure!(
            bytes.len() <= POINTER_LIMIT,
            "latest-validated manifest exceeds its byte bound"
        );
        let name = format!(".latest-validated-{}", uuid::Uuid::new_v4().simple());
        let file = fs::openat(
            &self.root.0,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        let mut stage = Stage {
            root: &self.root.0,
            name,
            active: true,
        };
        let mut file = File::from(file);
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.recheck(&release.reference)?;
        if self.previous.is_some() {
            fs::renameat(&self.root.0, stage.name.as_str(), &self.root.0, LATEST)?;
        } else {
            fs::renameat_with(
                &self.root.0,
                stage.name.as_str(),
                &self.root.0,
                LATEST,
                RenameFlags::NOREPLACE,
            )?;
        }
        stage.active = false;
        self.root.0.sync_all().context(
            "latest-validated manifest was replaced, but directory durability is uncertain",
        )?;
        Ok(())
    }
}

struct Stage<'a> {
    root: &'a File,
    name: String,
    active: bool,
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::unlinkat(self.root, self.name.as_str(), AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn reference(root: &Path, name: &str) -> VerifiedBundle {
        let bundle = root.join(name);
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join(ephemeris_tables::MANIFEST), b"derived").unwrap();
        std::fs::write(bundle.join(ephemeris_tables::SOURCE), b"source").unwrap();
        VerifiedBundle {
            snapshot_id: digest(b"synthetic"),
            from: chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            to_exclusive: chrono::NaiveDate::from_ymd_opt(2000, 1, 2).unwrap(),
            derived_sha256: digest(b"derived"),
            source_sha256: digest(b"source"),
            iceberg_sha256: None,
            duckdb_sha256: None,
        }
    }

    #[test]
    fn self_consistent_out_of_policy_prior_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut unsupported = reference(root, "prior");
        unsupported.from = chrono::NaiveDate::from_ymd_opt(1899, 12, 31).unwrap();
        unsupported.to_exclusive = chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
        let prior = ReleaseManifest::new("prior".into(), unsupported).unwrap();
        let latest = root.join(LATEST);
        let bytes = serde_json::to_vec_pretty(&prior).unwrap();
        std::fs::write(&latest, &bytes).unwrap();
        let before = std::fs::metadata(&latest).unwrap();
        reference(root, "candidate");
        let result = Promotion::begin(root, "candidate".into(), prior.release_id);
        assert!(matches!(result, Err(error) if error.to_string().contains("schema or identity")));
        let after = std::fs::metadata(&latest).unwrap();
        assert_eq!(std::fs::read(&latest).unwrap(), bytes);
        assert_eq!(after.mode(), before.mode());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    }

    #[test]
    fn shared_writable_root_refuses_before_pointer_change() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let prior = reference(root, "prior");
        Promotion::begin(root, "prior".into(), "absent".into())
            .unwrap()
            .commit(prior)
            .unwrap();
        let latest = root.join(LATEST);
        let bytes = std::fs::read(&latest).unwrap();
        let current = ReleaseManifest::parse(&bytes).unwrap().release_id;
        let before = std::fs::metadata(&latest).unwrap();
        reference(root, "candidate");
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o770)).unwrap();
        let result = Promotion::begin(root, "candidate".into(), current);
        assert!(matches!(result, Err(error) if error.to_string().contains("group or world")));
        let after = std::fs::metadata(&latest).unwrap();
        assert_eq!(std::fs::read(&latest).unwrap(), bytes);
        assert_eq!(after.mode(), before.mode());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    }

    #[test]
    fn competing_replacements_with_one_expectation_have_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let prior = reference(root, "prior");
        Promotion::begin(root, "prior".into(), "absent".into())
            .unwrap()
            .commit(prior)
            .unwrap();
        let current = ReleaseManifest::parse(&std::fs::read(root.join(LATEST)).unwrap())
            .unwrap()
            .release_id;
        let barrier = Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let handles = ["first", "second"].map(|name| {
                let reference = reference(root, name);
                let barrier = &barrier;
                let current = &current;
                scope.spawn(move || {
                    barrier.wait();
                    let result = Promotion::begin(root, name.into(), current.clone())
                        .and_then(|promotion| promotion.commit(reference));
                    (name, result)
                })
            });
            handles.map(|handle| handle.join().unwrap())
        });
        let release = ReleaseManifest::parse(&std::fs::read(root.join(LATEST)).unwrap()).unwrap();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        for (name, result) in outcomes {
            if let Err(error) = result {
                assert_ne!(release.bundle, name);
                let message = error.to_string();
                assert!(
                    message.contains("busy") || message.contains("expected current"),
                    "{message}"
                );
            } else {
                assert_eq!(release.bundle, name);
            }
        }
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 4);
    }

    #[test]
    fn completed_promotion_releases_lock_even_with_an_inherited_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let verified = reference(root, "candidate");
        let promotion = Promotion::begin(root, "candidate".into(), "absent".into()).unwrap();
        // An inherited descriptor can remain open between a concurrent fork and exec.
        let inherited = promotion.root.0.try_clone().unwrap();
        promotion.commit(verified).unwrap();
        let next = Promotion::begin(root, "candidate".into(), "absent".into());
        assert!(
            next.is_ok(),
            "completed operation still holds the directory lock"
        );
        drop(inherited);
    }

    #[test]
    fn changed_target_or_receipt_refuses_before_publishing() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let verified = reference(root, "candidate");
        let promotion = Promotion::begin(root, "candidate".into(), "absent".into()).unwrap();
        let inherited = promotion.root.0.try_clone().unwrap();
        std::fs::write(
            root.join("candidate").join(ephemeris_tables::MANIFEST),
            b"changed",
        )
        .unwrap();
        let error = promotion.commit(verified).unwrap_err();
        assert!(error
            .to_string()
            .contains("verified bundle receipt changed"));
        assert!(!root.join(LATEST).exists());
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);

        let verified = reference(root, "replacement");
        let promotion = Promotion::begin(root, "replacement".into(), "absent".into()).unwrap();
        drop(inherited);
        std::fs::rename(root.join("replacement"), root.join("held")).unwrap();
        reference(root, "replacement");
        let error = promotion.commit(verified).unwrap_err();
        assert!(error.to_string().contains("directory or bundle changed"));
        assert!(!root.join(LATEST).exists());
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 3);
    }
}
