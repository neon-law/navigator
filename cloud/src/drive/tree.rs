//! Parent-scoped Drive trees and provider access inspection.
use super::{
    DriveApiFile, DriveApiFiles, DriveError, DriveFile, DriveFolder, DriveService, GoogleDrive,
    FOLDER_MIME_TYPE,
};
use serde::Deserialize;
use std::collections::BTreeSet;

/// A file with its path relative to the inspected root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveTreeFile {
    pub relative_path: String,
    pub ancestor_ids: Vec<String>,
    pub file: DriveFile,
}

/// Every visited folder, including empty folders, with its containment chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveTreeFolder {
    pub id: String,
    pub relative_path: String,
    pub ancestor_ids: Vec<String>,
}
struct DriveTree {
    files: Vec<DriveTreeFile>,
    folders: Vec<DriveTreeFolder>,
}

/// Provider permissions are evidence; the store decides which people are authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrivePermissionAccess {
    pub id: String,
    pub permission_type: String,
    pub email: Option<String>,
    pub role: String,
    pub inherited: bool,
    pub direct: bool,
    pub view: Option<String>,
}

/// Limited access still permits organizers and directly granted members.
#[derive(Debug, Clone, PartialEq, Eq)]
// These are independent Google Drive capability flags, not domain states.
#[allow(clippy::struct_excessive_bools)]
pub struct DriveFolderAccess {
    pub inherited_permissions_disabled: bool,
    pub can_disable_inheritance: bool,
    pub can_enable_inheritance: bool,
    pub can_list_children: bool,
    pub parent_ids: Vec<String>,
    pub permissions: Vec<DrivePermissionAccess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveIsolationBlock {
    Unsupported,
    CapabilityUnavailable,
    ProviderDenied,
    UnverifiedState,
}

/// Restricted is a provider state, never a claim that its grantees are firm-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveIsolationOutcome {
    Restricted(DriveFolderAccess),
    Blocked(DriveIsolationBlock),
}

fn component(name: &str) -> Result<(), DriveError> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\', '\0']) {
        return Err(DriveError::TreeBoundary);
    }
    Ok(())
}

fn quoted_id(id: &str) -> Result<&str, DriveError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(DriveError::TreeBoundary);
    }
    Ok(id)
}

impl GoogleDrive {
    pub(super) async fn tree_allocate_id(&self) -> Result<String, DriveError> {
        #[derive(Deserialize)]
        struct Ids {
            ids: Vec<String>,
        }
        let ids = Self::checked(
            self.http
                .get(format!("{}/files/generateIds", self.base_url))
                .bearer_auth(self.token().await?)
                .query(&[("count", "1"), ("space", "drive"), ("type", "files")])
                .send()
                .await,
            "allocating folder identity",
        )?
        .json::<Ids>()
        .await
        .map_err(|source| DriveError::Response {
            action: "allocating folder identity",
            source,
        })?;
        let [id] = ids.ids.as_slice() else {
            return Err(DriveError::TreeBoundary);
        };
        quoted_id(id)?;
        Ok(id.clone())
    }
    pub(super) async fn tree_find_child(
        &self,
        parent: &str,
        name: &str,
    ) -> Result<Option<DriveFolder>, DriveError> {
        component(name)?;
        if !self.is_ingest_folder(parent).await? {
            return Err(DriveError::TreeBoundary);
        }
        let mut matches = self.tree_children(parent).await?.into_iter().filter(|f| {
            f.mime_type.as_deref() == Some(FOLDER_MIME_TYPE) && f.name.as_deref() == Some(name)
        });
        let first = matches.next();
        if matches.next().is_some() {
            return Err(DriveError::AmbiguousFolderName { count: 2 });
        }
        first
            .map(|f| f.into_folder().ok_or(DriveError::TreeBoundary))
            .transpose()
    }
    pub(super) async fn tree_ensure_reserved(
        &self,
        parent: &str,
        name: &str,
        id: &str,
    ) -> Result<DriveFolder, DriveError> {
        component(name)?;
        quoted_id(id)?;
        if !self.is_ingest_folder(parent).await? {
            return Err(DriveError::TreeBoundary);
        }
        let probe = self
            .http
            .get(self.file_url(id))
            .bearer_auth(self.token().await?)
            .query(&[
                ("supportsAllDrives", "true"),
                ("fields", "id,name,mimeType,driveId,parents,trashed"),
            ])
            .send()
            .await
            .map_err(|source| DriveError::Request {
                action: "reading reserved folder",
                source,
            })?;
        let response = if probe.status().as_u16() == 404 {
            let created=self.http.post(self.files_url()).bearer_auth(self.token().await?)
                .query(&[("supportsAllDrives","true"),("fields","id,name,mimeType,driveId,parents,trashed")])
                .json(&serde_json::json!({"id":id,"name":name,"mimeType":FOLDER_MIME_TYPE,"parents":[parent]}))
                .send().await.map_err(|source|DriveError::Request{action:"creating reserved folder",source})?;
            if created.status().as_u16() == 409 {
                Self::checked(
                    self.http
                        .get(self.file_url(id))
                        .bearer_auth(self.token().await?)
                        .query(&[
                            ("supportsAllDrives", "true"),
                            ("fields", "id,name,mimeType,driveId,parents,trashed"),
                        ])
                        .send()
                        .await,
                    "reconciling reserved folder",
                )?
            } else {
                Self::checked(Ok(created), "creating reserved folder")?
            }
        } else {
            Self::checked(Ok(probe), "reading reserved folder")?
        };
        let folder =
            response
                .json::<DriveApiFile>()
                .await
                .map_err(|source| DriveError::Response {
                    action: "checking reserved folder",
                    source,
                })?;
        if folder.id.as_deref() != Some(id)
            || folder.name.as_deref() != Some(name)
            || folder.mime_type.as_deref() != Some(FOLDER_MIME_TYPE)
            || folder.drive_id.as_deref() != Some(self.projects_drive_id.as_str())
            || folder.trashed == Some(true)
            || !folder
                .parents
                .as_ref()
                .is_some_and(|parents| parents.len() == 1 && parents[0] == parent)
        {
            return Err(DriveError::TreeBoundary);
        }
        folder.into_folder().ok_or(DriveError::TreeBoundary)
    }

    async fn tree_children(&self, parent: &str) -> Result<Vec<DriveApiFile>, DriveError> {
        quoted_id(parent)?;
        let metadata = Self::checked(
            self.http
                .get(self.file_url(parent))
                .bearer_auth(self.token().await?)
                .query(&[
                    ("supportsAllDrives", "true"),
                    ("fields", "capabilities(canListChildren)"),
                ])
                .send()
                .await,
            "checking tree listing capability",
        )?
        .json::<AccessMetadata>()
        .await
        .map_err(|source| DriveError::Response {
            action: "checking tree listing capability",
            source,
        })?;
        if !metadata.capabilities.can_list_children {
            return Err(DriveError::TreeBoundary);
        }
        let query = format!("'{parent}' in parents and trashed = false");
        let mut next = None;
        let mut seen = BTreeSet::new();
        let mut children = Vec::new();
        loop {
            let mut request = self.http.get(self.files_url()).bearer_auth(self.token().await?).query(&[
                ("q", query.as_str()), ("corpora", "drive"),
                ("driveId", self.projects_drive_id.as_str()),
                ("includeItemsFromAllDrives", "true"), ("supportsAllDrives", "true"),
                ("pageSize", "100"),
                ("fields", "nextPageToken,files(id,name,mimeType,size,modifiedTime,md5Checksum,version,driveId,parents,trashed)"),
            ]);
            if let Some(token) = next.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let page = Self::checked(request.send().await, "listing tree children")?
                .json::<DriveApiFiles>()
                .await
                .map_err(|source| DriveError::Response {
                    action: "listing tree children",
                    source,
                })?;
            children.extend(page.files);
            if children.len() > 5000 {
                return Err(DriveError::TreeBoundary);
            }
            next = page.next_page_token;
            match &next {
                None => break,
                Some(token) if seen.insert(token.clone()) => {}
                Some(_) => return Err(DriveError::TreeBoundary),
            }
        }
        Ok(children)
    }

    pub(super) async fn tree_ensure_child(
        &self,
        parent: &str,
        name: &str,
    ) -> Result<DriveFolder, DriveError> {
        component(name)?;
        if !self.is_ingest_folder(parent).await? {
            return Err(DriveError::TreeBoundary);
        }
        let mut matches = self.tree_children(parent).await?.into_iter().filter(|f| {
            f.mime_type.as_deref() == Some(FOLDER_MIME_TYPE) && f.name.as_deref() == Some(name)
        });
        let first = matches.next();
        if matches.next().is_some() {
            return Err(DriveError::AmbiguousFolderName { count: 2 });
        }
        if let Some(folder) = first {
            return folder.into_folder().ok_or(DriveError::TreeBoundary);
        }
        let response = Self::checked(self.http.post(self.files_url()).bearer_auth(self.token().await?)
            .query(&[("supportsAllDrives", "true"), ("fields", "id,name")])
            .json(&serde_json::json!({"name":name,"mimeType":FOLDER_MIME_TYPE,"parents":[parent]}))
            .send().await, "creating child folder")?;
        response
            .json::<DriveApiFile>()
            .await
            .map_err(|source| DriveError::Response {
                action: "creating child folder",
                source,
            })?
            .into_folder()
            .ok_or(DriveError::MissingFolderId {
                action: "creating child folder",
            })
    }

    async fn tree_walk(&self, root: &str) -> Result<DriveTree, DriveError> {
        if !self.is_ingest_folder(root).await? {
            return Err(DriveError::TreeBoundary);
        }
        let mut pending = vec![(
            root.to_string(),
            String::new(),
            0_usize,
            vec![root.to_string()],
        )];
        let mut seen = BTreeSet::new();
        let mut output = Vec::new();
        let mut folders = Vec::new();
        while let Some((parent, prefix, depth, ancestor_ids)) = pending.pop() {
            if depth > 32 || !seen.insert(parent.clone()) || seen.len() > 512 {
                return Err(DriveError::TreeBoundary);
            }
            folders.push(DriveTreeFolder {
                id: parent.clone(),
                relative_path: prefix.clone(),
                ancestor_ids: ancestor_ids.clone(),
            });
            for entry in self.tree_children(&parent).await? {
                if entry.drive_id.as_deref() != Some(self.projects_drive_id.as_str())
                    || entry.trashed == Some(true)
                    || !entry
                        .parents
                        .as_ref()
                        .is_some_and(|parents| parents.contains(&parent))
                {
                    return Err(DriveError::TreeBoundary);
                }
                let name = entry.name.as_deref().ok_or(DriveError::TreeBoundary)?;
                component(name)?;
                let relative_path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.mime_type.as_deref() == Some(FOLDER_MIME_TYPE) {
                    let child_id = entry.id.ok_or(DriveError::TreeBoundary)?;
                    let mut child_ancestors = ancestor_ids.clone();
                    child_ancestors.push(child_id.clone());
                    pending.push((child_id, relative_path, depth + 1, child_ancestors));
                } else {
                    let file = entry.into_file().ok_or(DriveError::TreeBoundary)?;
                    output.push(DriveTreeFile {
                        relative_path,
                        ancestor_ids: ancestor_ids.clone(),
                        file,
                    });
                    if output.len() > 5000 {
                        return Err(DriveError::TreeBoundary);
                    }
                }
            }
        }
        output.sort_by(|a, b| {
            a.relative_path
                .cmp(&b.relative_path)
                .then(a.file.id.cmp(&b.file.id))
        });
        folders.sort_by(|a, b| a.relative_path.cmp(&b.relative_path).then(a.id.cmp(&b.id)));
        Ok(DriveTree {
            files: output,
            folders,
        })
    }

    pub(super) async fn tree_list(&self, root: &str) -> Result<Vec<DriveTreeFile>, DriveError> {
        Ok(self
            .tree_walk(root)
            .await?
            .files
            .into_iter()
            .filter(|f| f.file.mime_type != "application/vnd.google-apps.shortcut")
            .collect())
    }
    pub(super) async fn tree_items(&self, root: &str) -> Result<Vec<DriveTreeFile>, DriveError> {
        Ok(self.tree_walk(root).await?.files)
    }
    pub(super) async fn tree_folders(
        &self,
        root: &str,
    ) -> Result<Vec<DriveTreeFolder>, DriveError> {
        Ok(self.tree_walk(root).await?.folders)
    }
    pub(super) async fn tree_access(&self, folder: &str) -> Result<DriveFolderAccess, DriveError> {
        if !self.is_ingest_folder(folder).await? {
            return Err(DriveError::TreeBoundary);
        }
        let metadata = Self::checked(
            self.http
                .get(self.file_url(folder))
                .bearer_auth(self.token().await?)
                .query(&[
                    ("supportsAllDrives", "true"),
                    (
                        "fields",
                        "parents,inheritedPermissionsDisabled,capabilities(canDisableInheritedPermissions,canEnableInheritedPermissions,canListChildren)",
                    ),
                ])
                .send()
                .await,
            "reading folder capabilities",
        )?
        .json::<AccessMetadata>()
        .await
        .map_err(|source| DriveError::Response {
            action: "reading folder capabilities",
            source,
        })?;
        let permissions = self.tree_permission_rows(folder).await?;
        Ok(DriveFolderAccess {
            inherited_permissions_disabled: metadata.inherited_permissions_disabled,
            can_disable_inheritance: metadata.capabilities.can_disable_inherited_permissions,
            can_enable_inheritance: metadata.capabilities.can_enable_inherited_permissions,
            can_list_children: metadata.capabilities.can_list_children,
            parent_ids: metadata.parents,
            permissions,
        })
    }

    async fn tree_permission_rows(
        &self,
        folder: &str,
    ) -> Result<Vec<DrivePermissionAccess>, DriveError> {
        let mut permissions = Vec::new();
        let mut next = None;
        let mut seen = BTreeSet::new();
        loop {
            let mut request = self.http.get(self.permissions_url(folder)).bearer_auth(self.token().await?)
                .query(&[("supportsAllDrives","true"),("pageSize","100"),
                ("fields","nextPageToken,permissions(id,type,emailAddress,role,view,permissionDetails(inherited))")]);
            if let Some(token) = next.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let page = Self::checked(request.send().await, "inspecting folder permissions")?
                .json::<AccessPermissions>()
                .await
                .map_err(|source| DriveError::Response {
                    action: "inspecting folder permissions",
                    source,
                })?;
            for permission in page.permissions {
                permissions.push(DrivePermissionAccess {
                    id: permission.id,
                    permission_type: permission.kind,
                    email: permission.email_address,
                    role: permission.role,
                    view: permission.view,
                    direct: permission.permission_details.iter().any(|p| !p.inherited),
                    inherited: permission.permission_details.iter().any(|p| p.inherited),
                });
            }
            if permissions.len() > 5000 {
                return Err(DriveError::TreeBoundary);
            }
            next = page.next_page_token;
            match &next {
                None => break,
                Some(token) if seen.insert(token.clone()) => {}
                Some(_) => return Err(DriveError::TreeBoundary),
            }
        }
        Ok(permissions)
    }
    pub(super) async fn tree_item_permissions(
        &self,
        id: &str,
    ) -> Result<Vec<DrivePermissionAccess>, DriveError> {
        quoted_id(id)?;
        let item = Self::checked(
            self.http
                .get(self.file_url(id))
                .bearer_auth(self.token().await?)
                .query(&[
                    ("supportsAllDrives", "true"),
                    ("fields", "id,driveId,trashed"),
                ])
                .send()
                .await,
            "checking item drive",
        )?
        .json::<DriveApiFile>()
        .await
        .map_err(|source| DriveError::Response {
            action: "checking item drive",
            source,
        })?;
        if item.id.as_deref() != Some(id)
            || item.drive_id.as_deref() != Some(self.projects_drive_id.as_str())
            || item.trashed == Some(true)
        {
            return Err(DriveError::TreeBoundary);
        }
        self.tree_permission_rows(id).await
    }

    pub(super) async fn tree_inherit(
        &self,
        folder: &str,
    ) -> Result<Option<DriveFolderAccess>, DriveError> {
        let before = self.tree_access(folder).await?;
        if !before.inherited_permissions_disabled {
            return Ok(Some(before));
        }
        if !before.can_enable_inheritance {
            return Ok(None);
        }
        let response = self
            .http
            .patch(self.file_url(folder))
            .bearer_auth(self.token().await?)
            .query(&[("supportsAllDrives", "true")])
            .json(&serde_json::json!({"inheritedPermissionsDisabled":false}))
            .send()
            .await
            .map_err(|source| DriveError::Request {
                action: "enabling folder inheritance",
                source,
            })?;
        if matches!(response.status().as_u16(), 403 | 405) {
            return Ok(None);
        }
        Self::checked(Ok(response), "enabling folder inheritance")?;
        let after = self.tree_access(folder).await?;
        Ok((!after.inherited_permissions_disabled && after.can_list_children).then_some(after))
    }

    pub(super) async fn tree_restrict(
        &self,
        folder: &str,
    ) -> Result<DriveIsolationOutcome, DriveError> {
        let before = match self.tree_access(folder).await {
            Ok(access) => access,
            Err(DriveError::Api { status: 403, .. }) => {
                return Ok(DriveIsolationOutcome::Blocked(
                    DriveIsolationBlock::ProviderDenied,
                ));
            }
            Err(error) => return Err(error),
        };
        if before.inherited_permissions_disabled {
            return Ok(DriveIsolationOutcome::Restricted(before));
        }
        if !before.can_disable_inheritance {
            return Ok(DriveIsolationOutcome::Blocked(
                DriveIsolationBlock::CapabilityUnavailable,
            ));
        }
        let response = self
            .http
            .patch(self.file_url(folder))
            .bearer_auth(self.token().await?)
            .query(&[("supportsAllDrives", "true"), ("fields", "id")])
            .json(&serde_json::json!({"inheritedPermissionsDisabled":true}))
            .send()
            .await
            .map_err(|source| DriveError::Request {
                action: "restricting folder inheritance",
                source,
            })?;
        if matches!(response.status().as_u16(), 403 | 405) {
            return Ok(DriveIsolationOutcome::Blocked(
                DriveIsolationBlock::ProviderDenied,
            ));
        }
        Self::checked(Ok(response), "restricting folder inheritance")?;
        let after = match self.tree_access(folder).await {
            Ok(access) => access,
            Err(DriveError::Api { status: 403, .. }) => {
                return Ok(DriveIsolationOutcome::Blocked(
                    DriveIsolationBlock::ProviderDenied,
                ));
            }
            Err(error) => return Err(error),
        };
        Ok(if after.inherited_permissions_disabled {
            DriveIsolationOutcome::Restricted(after)
        } else {
            DriveIsolationOutcome::Blocked(DriveIsolationBlock::UnverifiedState)
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AccessCapabilities {
    #[serde(default)]
    can_disable_inherited_permissions: bool,
    #[serde(default)]
    can_enable_inherited_permissions: bool,
    #[serde(default)]
    can_list_children: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessMetadata {
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    inherited_permissions_disabled: bool,
    #[serde(default)]
    capabilities: AccessCapabilities,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessPermission {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    email_address: Option<String>,
    role: String,
    #[serde(default)]
    permission_details: Vec<AccessDetail>,
    #[serde(default)]
    view: Option<String>,
}
#[derive(Deserialize)]
struct AccessDetail {
    #[serde(default)]
    inherited: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessPermissions {
    #[serde(default)]
    permissions: Vec<AccessPermission>,
    next_page_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::{DriveMember, DriveMemberKind, DriveRole, FakeDrive};
    use async_trait::async_trait;
    use std::sync::Arc;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Debug)]
    struct Token;
    #[async_trait]
    impl google_cloud_token::TokenSource for Token {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer test-token".to_string())
        }
    }
    fn google(server: &MockServer) -> GoogleDrive {
        GoogleDrive::from_parts("shared-drive".to_string(), Arc::new(Token), &server.uri())
    }
    fn metadata(limited: bool, capable: bool) -> serde_json::Value {
        serde_json::json!({"id":"root","mimeType":FOLDER_MIME_TYPE,"driveId":"shared-drive","trashed":false,
            "inheritedPermissionsDisabled":limited,"capabilities":{"canDisableInheritedPermissions":capable,"canEnableInheritedPermissions":capable,"canListChildren":true}})
    }
    async fn permissions(server: &MockServer) {
        Mock::given(method("GET")).and(path("/files/root/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"permissions":[
                {"id":"organizer-id","type":"user","emailAddress":"organizer@example.com","role":"organizer","permissionDetails":[{"inherited":true}]}
            ]}))).mount(server).await;
    }

    #[tokio::test]
    async fn google_enables_inheritance_with_authoritative_readback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(true, true)))
            .with_priority(1)
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .with_priority(2)
            .expect(2)
            .mount(&server)
            .await;
        permissions(&server).await;
        Mock::given(method("PATCH"))
            .and(path("/files/root"))
            .and(body_json(
                serde_json::json!({"inheritedPermissionsDisabled":false}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"root"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = google(&server)
            .inherit_folder("root")
            .await
            .unwrap()
            .unwrap();
        assert!(!result.inherited_permissions_disabled);
        server.verify().await;
    }
    #[tokio::test]
    async fn poisoned_new_fake_tree_operations_refuse_without_panicking() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("synthetic").await.unwrap();
        let clone = drive.clone();
        assert!(std::thread::spawn(move || {
            let _guard = clone.state.lock().unwrap();
            panic!("poison fixture");
        })
        .join()
        .is_err());
        assert!(matches!(
            drive.folder_access(&root.id).await,
            Err(DriveError::StateUnavailable)
        ));
        assert!(!drive.block_folder_restriction(&root.id));
    }
    #[tokio::test]
    async fn fake_children_preserve_paths_and_do_not_enter_project_root_listing() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("synthetic").await.unwrap();
        let first = drive
            .ensure_child_folder(&root.id, "Work Product")
            .await
            .unwrap();
        assert_eq!(
            first,
            drive
                .ensure_child_folder(&root.id, "Work Product")
                .await
                .unwrap()
        );
        let second = drive
            .ensure_child_folder(&root.id, "Client Production")
            .await
            .unwrap();
        assert!(drive
            .add_file(&first.id, "same.txt", "text/plain", b"one".to_vec(), None)
            .is_some());
        assert!(drive
            .add_file(&second.id, "same.txt", "text/plain", b"two".to_vec(), None)
            .is_some());
        let files = drive.list_tree(&root.id).await.unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| f.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["Client Production/same.txt", "Work Product/same.txt"]
        );
        assert_eq!(drive.list_folders().await.unwrap(), vec![root]);
        assert!(drive
            .ensure_child_folder(&first.id, "../escape")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn fake_restriction_blocks_without_capability_and_retains_direct_grants() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("synthetic").await.unwrap();
        let member = DriveMember {
            kind: DriveMemberKind::User,
            email: "lawyer@example.com".to_string(),
        };
        drive
            .set_member_permission(&root.id, &member, DriveRole::Writer)
            .await
            .unwrap();
        assert!(drive.block_folder_restriction(&root.id));
        assert_eq!(
            drive.restrict_folder(&root.id).await.unwrap(),
            DriveIsolationOutcome::Blocked(DriveIsolationBlock::CapabilityUnavailable)
        );
        assert!(
            !drive
                .folder_access(&root.id)
                .await
                .unwrap()
                .inherited_permissions_disabled
        );
        assert_eq!(drive.members(&root.id), vec![(member, DriveRole::Writer)]);
    }

    #[tokio::test]
    async fn google_capability_refusal_never_patches_or_removes_membership() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, false)))
            .mount(&server)
            .await;
        permissions(&server).await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(
            google(&server).restrict_folder("root").await.unwrap(),
            DriveIsolationOutcome::Blocked(DriveIsolationBlock::CapabilityUnavailable)
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn google_restriction_rechecks_provider_state_and_reports_organizer_access() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .with_priority(1)
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(true, true)))
            .mount(&server)
            .await;
        permissions(&server).await;
        Mock::given(method("PATCH"))
            .and(path("/files/root"))
            .and(query_param("supportsAllDrives", "true"))
            .and(body_json(
                serde_json::json!({"inheritedPermissionsDisabled":true}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"root"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let DriveIsolationOutcome::Restricted(access) =
            google(&server).restrict_folder("root").await.unwrap()
        else {
            panic!("provider confirmed limited access");
        };
        assert!(access.inherited_permissions_disabled);
        assert!(access
            .permissions
            .iter()
            .any(|p| p.role == "organizer" && p.inherited));
        server.verify().await;
    }

    #[tokio::test]
    async fn google_walk_follows_pages_and_children_but_never_shortcuts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/child"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .mount(&server)
            .await;
        let root_query = "'root' in parents and trashed = false";
        Mock::given(method("GET")).and(path("/files")).and(query_param("q",root_query))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"nextPageToken":"page-two","files":[
                {"id":"child","name":"Work Product","mimeType":FOLDER_MIME_TYPE,"driveId":"shared-drive","parents":["root"]}
            ]}))).with_priority(1)
            .up_to_n_times(1).expect(1).mount(&server).await;
        Mock::given(method("GET")).and(path("/files")).and(query_param("q",root_query)).and(query_param("pageToken","page-two"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files":[
                {"id":"shortcut","name":"Other matter","mimeType":"application/vnd.google-apps.shortcut","driveId":"shared-drive","parents":["root"]},
                {"id":"file-one","name":"same.txt","mimeType":"text/plain","driveId":"shared-drive","parents":["root"]}
            ]}))).expect(1).mount(&server).await;
        Mock::given(method("GET")).and(path("/files")).and(query_param("q","'child' in parents and trashed = false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files":[
                {"id":"file-two","name":"same.txt","mimeType":"text/plain","driveId":"shared-drive","parents":["child"]}
            ]}))).expect(1).mount(&server).await;
        let files = google(&server).list_tree("root").await.unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| f.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["Work Product/same.txt", "same.txt"]
        );
        server.verify().await;
    }
    #[tokio::test]
    async fn google_child_creation_is_bound_to_verified_parent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("q", "'root' in parents and trashed = false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files":[]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/files"))
            .and(query_param("supportsAllDrives","true"))
            .and(body_json(serde_json::json!({"name":"Work Product","mimeType":FOLDER_MIME_TYPE,"parents":["root"]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"child","name":"Work Product"})))
            .expect(1).mount(&server).await;
        assert_eq!(
            google(&server)
                .ensure_child_folder("root", "Work Product")
                .await
                .unwrap(),
            DriveFolder {
                id: "child".to_string(),
                name: "Work Product".to_string()
            }
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn google_mutation_denial_and_unconfirmed_write_are_blocked() {
        for (status, expected) in [
            (403, DriveIsolationBlock::ProviderDenied),
            (200, DriveIsolationBlock::UnverifiedState),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/files/root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
                .mount(&server)
                .await;
            permissions(&server).await;
            Mock::given(method("PATCH"))
                .and(path("/files/root"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_json(serde_json::json!({"id":"root"})),
                )
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                google(&server).restrict_folder("root").await.unwrap(),
                DriveIsolationOutcome::Blocked(expected)
            );
            server.verify().await;
        }
    }
    #[tokio::test]
    async fn reserved_folder_retry_verifies_conflict_and_never_allocates_a_second_identity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/reserved"))
            .respond_with(ResponseTemplate::new(404))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/files/reserved"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"reserved","name":"Work Product","mimeType":FOLDER_MIME_TYPE,"driveId":"shared-drive","parents":["root"],"trashed":false})))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/files"))
            .and(body_json(serde_json::json!({"id":"reserved","name":"Work Product","mimeType":FOLDER_MIME_TYPE,"parents":["root"]})))
            .respond_with(ResponseTemplate::new(409)).expect(1).mount(&server).await;
        let drive = google(&server);
        let first = drive
            .ensure_child_folder_with_id("root", "Work Product", "reserved")
            .await
            .unwrap();
        assert_eq!(
            first,
            drive
                .ensure_child_folder_with_id("root", "Work Product", "reserved")
                .await
                .unwrap()
        );
        assert_eq!(first.id, "reserved");
        server.verify().await;
    }
    #[tokio::test]
    async fn reserved_identity_bound_to_another_parent_is_never_adopted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(false, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/files/reserved"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"reserved","name":"Work Product","mimeType":FOLDER_MIME_TYPE,"driveId":"shared-drive","parents":["other-root"]})))
            .mount(&server).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert!(google(&server)
            .ensure_child_folder_with_id("root", "Work Product", "reserved")
            .await
            .is_err());
        server.verify().await;
    }
    #[tokio::test]
    async fn empty_custom_folders_and_file_ancestry_are_available_for_policy_audit() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("root").await.unwrap();
        let empty = drive
            .ensure_child_folder(&root.id, "Empty custom")
            .await
            .unwrap();
        assert!(drive.list_tree(&root.id).await.unwrap().is_empty());
        let folders = drive.list_tree_folders(&root.id).await.unwrap();
        assert_eq!(folders.len(), 2);
        assert!(folders.iter().any(|f| f.id == empty.id));
        let file = drive
            .add_file(
                &empty.id,
                "memo.txt",
                "text/plain",
                b"synthetic".to_vec(),
                None,
            )
            .unwrap();
        let files = drive.list_tree(&root.id).await.unwrap();
        assert_eq!(files[0].ancestor_ids, vec![root.id, empty.id]);
        drive
            .set_member_permission(
                &file.id,
                &DriveMember {
                    kind: DriveMemberKind::User,
                    email: "client@example.com".into(),
                },
                DriveRole::Reader,
            )
            .await
            .unwrap();
        let grants = drive.item_permissions(&file.id).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert!(grants[0].direct);
    }
    #[tokio::test]
    async fn direct_grant_never_updates_inherited_or_organizer_membership() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/root"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata(true, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/files/root/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"permissions":[
                {"id":"owner","type":"user","emailAddress":"organizer@example.com","role":"organizer","permissionDetails":[{"inherited":true}]},
                {"id":"inherited","type":"user","emailAddress":"lawyer@example.com","role":"reader","permissionDetails":[{"inherited":true}]}
            ]}))).mount(&server).await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/files/root/permissions"))
            .and(body_json(serde_json::json!({"type":"user","role":"writer","emailAddress":"lawyer@example.com"})))
            .respond_with(ResponseTemplate::new(200)).expect(1).mount(&server).await;
        let drive = google(&server);
        for address in ["organizer@example.com", "lawyer@example.com"] {
            drive
                .ensure_direct_member_permission(
                    "root",
                    &DriveMember {
                        kind: DriveMemberKind::User,
                        email: address.into(),
                    },
                    DriveRole::Writer,
                )
                .await
                .unwrap();
        }
    }
}
