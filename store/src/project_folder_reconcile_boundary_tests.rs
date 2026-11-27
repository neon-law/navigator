// Provider acknowledgments are not access proof: audit the final effective grants.
struct AclScript {
    folders: std::sync::Mutex<
        std::collections::VecDeque<Result<DriveFolderAccess, cloud::drive::DriveError>>,
    >,
    files: std::sync::Mutex<std::collections::VecDeque<Vec<DrivePermissionAccess>>>,
    restricted: std::sync::Mutex<Option<cloud::drive::DriveIsolationOutcome>>,
    inherited: Option<DriveFolderAccess>,
    ensured: std::sync::Mutex<Vec<String>>,
    removed: std::sync::Mutex<Vec<String>>,
}
impl AclScript {
    fn new(reads: Vec<Result<DriveFolderAccess, cloud::drive::DriveError>>) -> Self {
        Self {
            folders: std::sync::Mutex::new(reads.into()),
            files: std::sync::Mutex::new(std::collections::VecDeque::default()),
            restricted: std::sync::Mutex::new(None),
            inherited: None,
            ensured: std::sync::Mutex::new(Vec::new()),
            removed: std::sync::Mutex::new(Vec::new()),
        }
    }
}
#[async_trait::async_trait]
impl DriveService for AclScript {
    async fn create_folder(&self, _code: &str) -> Result<cloud::DriveFolder, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn find_folder_by_name(
        &self,
        _code: &str,
    ) -> Result<Option<cloud::DriveFolder>, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn list_folders(&self) -> Result<Vec<cloud::DriveFolder>, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn is_ingest_folder(&self, _id: &str) -> Result<bool, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn list_files(&self, _id: &str) -> Result<Vec<cloud::DriveFile>, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn download_file(
        &self,
        _id: &str,
    ) -> Result<cloud::DriveFileDownload, cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn set_member_permission(
        &self,
        _id: &str,
        _member: &DriveMember,
        _role: DriveRole,
    ) -> Result<(), cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn move_to_archive(&self, _id: &str) -> Result<(), cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn restore_from_archive(&self, _id: &str) -> Result<(), cloud::DriveError> {
        Err(cloud::DriveError::UnsupportedTree)
    }
    async fn folder_access(&self, _id: &str) -> Result<DriveFolderAccess, cloud::DriveError> {
        self.folders
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(cloud::DriveError::StateUnavailable))
    }
    async fn restrict_folder(
        &self,
        _id: &str,
    ) -> Result<cloud::drive::DriveIsolationOutcome, cloud::DriveError> {
        Ok(self.restricted.lock().unwrap().take().unwrap_or(
            cloud::drive::DriveIsolationOutcome::Blocked(
                cloud::drive::DriveIsolationBlock::Unsupported,
            ),
        ))
    }
    async fn ensure_direct_member_permission(
        &self,
        _id: &str,
        member: &DriveMember,
        role: DriveRole,
    ) -> Result<(), cloud::DriveError> {
        assert_eq!(role, DriveRole::Writer);
        self.ensured.lock().unwrap().push(member.email.clone());
        Ok(())
    }
    async fn remove_member_permission(
        &self,
        _id: &str,
        member: &DriveMember,
    ) -> Result<(), cloud::DriveError> {
        self.removed.lock().unwrap().push(member.email.clone());
        Ok(())
    }
    async fn item_permissions(
        &self,
        _id: &str,
    ) -> Result<Vec<DrivePermissionAccess>, cloud::DriveError> {
        self.files
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(cloud::DriveError::StateUnavailable)
    }
    async fn inherit_folder(
        &self,
        _id: &str,
    ) -> Result<Option<DriveFolderAccess>, cloud::DriveError> {
        Ok(self.inherited.clone())
    }
}
fn acl(permissions: Vec<DrivePermissionAccess>, limited: bool) -> DriveFolderAccess {
    DriveFolderAccess {
        inherited_permissions_disabled: limited,
        can_disable_inheritance: true,
        can_enable_inheritance: true,
        can_list_children: true,
        parent_ids: vec!["parent".into()],
        permissions,
    }
}
fn grant(address: &str, direct: bool, inherited: bool) -> DrivePermissionAccess {
    DrivePermissionAccess {
        id: format!("synthetic-{address}"),
        permission_type: "user".into(),
        email: Some(address.into()),
        role: "writer".into(),
        direct,
        inherited,
        view: None,
    }
}

#[tokio::test]
async fn unsupported_acl_and_provider_failure_remain_distinct_and_never_grant_access() {
    let unsupported = AclScript::new(vec![Err(cloud::DriveError::UnsupportedTree)]);
    assert_eq!(
        secure(&unsupported, "folder", &members(), false, true)
            .await
            .unwrap(),
        Err(FolderBlock::UnsupportedProvider)
    );
    assert!(unsupported.ensured.lock().unwrap().is_empty());
    let outage = AclScript::new(vec![Err(cloud::DriveError::Authentication)]);
    assert!(matches!(
        secure(&outage, "folder", &members(), false, true).await,
        Err(FolderReconcileError::Provider(
            cloud::DriveError::Authentication
        ))
    ));
    assert!(outage.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn final_acl_must_contain_every_required_writer_and_exclude_removed_clients() {
    let mut roster = members();
    roster.trusted.insert("service@example.com".into());
    let approved = vec![
        grant("lawyer@example.com", true, false),
        grant("service@example.com", true, false),
        grant("client@example.com", true, false),
    ];
    let success = AclScript::new(vec![
        Ok(acl(Vec::new(), false)),
        Ok(acl(approved.clone(), true)),
    ]);
    *success.restricted.lock().unwrap() = Some(cloud::drive::DriveIsolationOutcome::Restricted(
        acl(approved.clone(), true),
    ));
    assert_eq!(
        secure(&success, "intake", &roster, true, false)
            .await
            .unwrap(),
        Ok(())
    );
    let ensured = success
        .ensured
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ensured,
        BTreeSet::from([
            "lawyer@example.com".into(),
            "service@example.com".into(),
            "client@example.com".into()
        ])
    );
    assert!(success.removed.lock().unwrap().is_empty());

    let missing = AclScript::new(vec![Ok(acl(Vec::new(), false)), Ok(acl(Vec::new(), true))]);
    *missing.restricted.lock().unwrap() = Some(cloud::drive::DriveIsolationOutcome::Restricted(
        acl(Vec::new(), true),
    ));
    assert_eq!(
        secure(&missing, "root", &roster, false, true)
            .await
            .unwrap(),
        Err(FolderBlock::ProviderCapability)
    );

    // A provider may acknowledge removal without actually removing access.
    let retained = AclScript::new(vec![
        Ok(acl(approved.clone(), true)),
        Ok(acl(approved.clone(), true)),
    ]);
    *retained.restricted.lock().unwrap() = Some(cloud::drive::DriveIsolationOutcome::Restricted(
        acl(approved, true),
    ));
    assert_eq!(
        secure(&retained, "root", &roster, false, true)
            .await
            .unwrap(),
        Err(FolderBlock::ProviderCapability)
    );
    assert_eq!(*retained.removed.lock().unwrap(), ["client@example.com"]);
}

#[tokio::test]
async fn file_content_grants_are_not_confused_with_metadata_visibility_or_removal_acknowledgments()
{
    let inherited = grant("outside@example.com", false, true);
    let script = AclScript::new(Vec::new());
    *script.files.lock().unwrap() = vec![vec![inherited.clone()], vec![inherited.clone()]].into();
    assert_eq!(
        secure_file(&script, "file", &members(), false)
            .await
            .unwrap(),
        Err(FolderBlock::UntrustedPermission)
    );
    assert!(script.removed.lock().unwrap().is_empty());
    let mut metadata_only = inherited;
    metadata_only.view = Some("metadata".into());
    let script = AclScript::new(Vec::new());
    *script.files.lock().unwrap() = vec![vec![metadata_only.clone()], vec![metadata_only]].into();
    assert_eq!(
        secure_file(&script, "file", &members(), false)
            .await
            .unwrap(),
        Ok(())
    );
    let client = grant("client@example.com", true, false);
    let script = AclScript::new(Vec::new());
    *script.files.lock().unwrap() = vec![vec![client.clone()], vec![client]].into();
    assert_eq!(
        secure_file(&script, "file", &members(), false)
            .await
            .unwrap(),
        Err(FolderBlock::ProviderCapability)
    );
    assert_eq!(*script.removed.lock().unwrap(), ["client@example.com"]);
}

#[tokio::test]
async fn descendant_capability_and_inherited_client_membership_are_verified_independently() {
    let script = AclScript::new(vec![Ok(acl(
        vec![grant("lawyer@example.com", true, false)],
        true,
    ))]);
    assert_eq!(
        secure_descendant(&script, "child", &members(), false)
            .await
            .unwrap(),
        Err(FolderBlock::ProviderCapability)
    );
    assert!(script.removed.lock().unwrap().is_empty());
    let firm = grant("lawyer@example.com", false, true);
    let root_only = acl(vec![firm.clone()], false);
    assert_eq!(
        inspect_descendant(&root_only, &members(), true),
        Err(FolderBlock::ProviderCapability)
    );
    let intake = acl(
        vec![firm.clone(), grant("client@example.com", false, true)],
        false,
    );
    assert_eq!(inspect_descendant(&intake, &members(), true), Ok(()));
    assert_eq!(
        inspect_descendant(&intake, &members(), false),
        Err(FolderBlock::UntrustedPermission)
    );
    let mut metadata_only = grant("outside@example.com", false, true);
    metadata_only.view = Some("metadata".into());
    assert_eq!(
        inspect_descendant(&acl(vec![firm, metadata_only], false), &members(), false),
        Ok(())
    );
}





#[tokio::test]
async fn nightly_journal_transport_failure_stops_before_later_projects() {
    let visited = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let first = Uuid::now_v7();
    let later = Uuid::now_v7();
    let observed = visited.clone();
    let result = visit_nightly_roster([first, later], move |id| {
        observed.lock().unwrap().push(id);
        std::future::ready(Err::<NightlyFolderOutcome, _>("journal unavailable"))
    })
    .await;
    assert_eq!(result, Err("journal unavailable"));
    assert_eq!(*visited.lock().unwrap(), [first]);
}
