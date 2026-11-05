//! Storage test doubles for the archives crate.
//!
//! [`CollisionStorage`] reproduces the two bucket shapes the snapshot
//! machinery can meet: a retention-locked bucket (create-only — plain
//! writes and deletes are panics, because no archive code path may reach
//! them) and the unlocked overwrite lane. Its `retention_locked` reports
//! which mode it is in, driving [`crate::runner`]'s destination choice the
//! way a real GCS bucket policy does.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use cloud::{ImmutableObject, ObjectHead, ObjectListing, StorageError, StorageService, StoredObject};

#[derive(Clone)]
pub struct CollisionStorage {
    locked: bool,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl CollisionStorage {
    /// A retention-locked bucket: `put_immutable` is create-only, and
    /// reaching for a plain `put` or a `delete` panics.
    pub fn locked() -> Arc<Self> {
        Self::new(true)
    }

    /// The unlocked overwrite lane: plain `put` replaces freely.
    pub fn unlocked() -> Arc<Self> {
        Self::new(false)
    }

    fn new(locked: bool) -> Arc<Self> {
        Arc::new(Self {
            locked,
            objects: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// Whether `key` is present — one step above probing the inner map in
    /// every assertion that names keys.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.objects.lock().unwrap().contains_key(key)
    }

    /// Every stored key, sorted (the map is a `BTreeMap`).
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }
}

#[async_trait]
impl StorageService for CollisionStorage {
    async fn put(&self, key: &str, bytes: &[u8], _content_type: &str) -> Result<(), StorageError> {
        if self.locked {
            panic!("plain write to a retention-locked bucket: {key}");
        }
        self.objects
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
        self.objects.lock().unwrap().get(key).cloned().map(|bytes| {
            StoredObject {
                key: key.to_string(),
                bytes,
                content_type: "application/octet-stream".into(),
            }
        }).ok_or_else(|| StorageError::NotFound(key.to_string()))
    }

    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        _content_type: &str,
    ) -> Result<ImmutableObject, StorageError> {
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(key) {
            return Err(StorageError::Unsupported(
                "create-only collision: object already exists",
            ));
        }
        objects.insert(key.to_string(), bytes.to_vec());
        Ok(ImmutableObject {
            generation: 1,
            size_bytes: bytes.len() as u64,
        })
    }

    /// No archive code path may delete on either lane — the double panics
    /// so a delete is a test failure at the call site.
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        panic!("delete called on the archive test double: {key}");
    }

    /// Mirrors [`cloud::FsStorage`]: the double has no signing surface.
    async fn signed_url(&self, key: &str, _ttl: std::time::Duration) -> Result<String, StorageError> {
        let _ = key;
        Err(StorageError::Unsupported("signed URLs"))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectListing>, StorageError> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .range(prefix.to_string()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, bytes)| ObjectListing {
                key: key.clone(),
                size_bytes: bytes.len() as u64,
            })
            .collect())
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectHead>, StorageError> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(key)
            .map(|bytes| ObjectHead {
                size_bytes: bytes.len() as u64,
            }))
    }

    async fn retention_locked(&self) -> Result<bool, StorageError> {
        Ok(self.locked)
    }
}
