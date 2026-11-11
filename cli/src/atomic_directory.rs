//! Atomic no-replacement publication of a verified directory.

use anyhow::{Context, Result};
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn publish_no_clobber(source: &Path, destination: &Path) -> Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .context("publish verified restore without replacing an existing path")?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn publish_no_clobber(_source: &Path, _destination: &Path) -> Result<()> {
    anyhow::bail!("no-clobber directory publication is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    #[test]
    fn simultaneous_publication_keeps_one_winner_and_preserves_the_loser() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("completed");
        let barrier = std::sync::Barrier::new(3);
        let outcomes = std::thread::scope(|scope| {
            let handles = ["first", "second"].map(|name| {
                let stage = parent.path().join(name);
                std::fs::create_dir(&stage).unwrap();
                std::fs::write(stage.join("marker"), name).unwrap();
                let destination = &destination;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    (
                        name,
                        stage.clone(),
                        super::publish_no_clobber(&stage, destination).is_ok(),
                    )
                })
            });
            barrier.wait();
            handles.map(|h| h.join().unwrap())
        });
        assert_eq!(outcomes.iter().filter(|(_, _, won)| *won).count(), 1);
        for (name, stage, won) in outcomes {
            if won {
                assert_eq!(
                    std::fs::read(destination.join("marker")).unwrap(),
                    name.as_bytes()
                );
                assert!(!stage.exists());
            } else {
                assert_eq!(
                    std::fs::read(stage.join("marker")).unwrap(),
                    name.as_bytes()
                );
            }
        }
    }
}
