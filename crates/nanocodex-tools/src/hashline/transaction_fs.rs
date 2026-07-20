//! Safe descriptor-relative adaptation of the `OpenAI` Codex Hashline Linux
//! filesystem capability. See the crate `NOTICE` for source provenance.

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Component, Path, PathBuf};
    use std::sync::Arc;

    use rustix::fs::{self, AtFlags, Dir, FlockOperation, IFlags, Mode, OFlags, ResolveFlags};
    use serde::{Deserialize, Serialize};

    use super::super::{
        FunctionCallError, MAX_MUTATIONS, MAX_TOTAL_BYTES, Observed, PreparedMutation,
        decode_bytes, encode_bytes, exact_digest, model_error,
    };

    const EXT_SUPER_MAGIC: i64 = 0xef53;
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    const CASEFOLD_FLAG: u32 = 0x4000_0000;
    const STATE_DIRECTORY: &str = ".nanocodex";
    const TRANSACTION_DIRECTORY: &str = "hashline-transactions";
    const JOURNAL_VERSION: u32 = 1;

    #[derive(Clone, Debug)]
    pub(in crate::hashline) struct NativeRoot {
        directory: Arc<File>,
        identity: Vec<u8>,
    }

    #[derive(Clone, Debug)]
    struct NativePath {
        root: Arc<File>,
        parent: Arc<File>,
        parent_relative: PathBuf,
        parent_identity: DirectoryIdentity,
        name: OsString,
        model_path: String,
    }

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    struct DirectoryIdentity {
        device: u64,
        inode: u64,
    }

    #[derive(Debug)]
    pub(in crate::hashline) struct TransactionLease {
        root_identity: Vec<u8>,
        covered_parents: BTreeSet<DirectoryIdentity>,
        _locks: Vec<File>,
    }

    #[derive(Debug)]
    pub(in crate::hashline) struct JournalHandle {
        directory: Arc<File>,
        file: File,
        name: String,
        journal: Journal,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Journal {
        version: u32,
        transaction_id: String,
        applied: usize,
        mutations: Vec<JournalMutation>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct JournalMutation {
        kind: String,
        path: String,
        destination: Option<String>,
        before: Option<String>,
        after: Option<String>,
    }

    pub(in crate::hashline) fn open_root(
        workspace: &Path,
        root_name: &str,
    ) -> Result<NativeRoot, FunctionCallError> {
        let workspace =
            File::open(workspace).map_err(|error| io_error("open transaction workspace", error))?;
        if !workspace
            .metadata()
            .map_err(|error| io_error("inspect transaction workspace", error))?
            .is_dir()
        {
            return model_error("transaction workspace is not a directory");
        }
        let directory = if root_name == "." {
            workspace
        } else {
            validate_relative(root_name)?;
            let fd = fs::openat2(
                &workspace,
                root_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
            )
            .map_err(|error| io_error("open transaction root", error))?;
            File::from(fd)
        };
        ensure_supported_directory(&directory)?;
        let identity = identity_bytes(
            &directory
                .metadata()
                .map_err(|error| io_error("inspect transaction root", error))?,
        );
        Ok(NativeRoot {
            directory: Arc::new(directory),
            identity,
        })
    }

    pub(in crate::hashline) fn root_identity(root: &NativeRoot) -> &[u8] {
        &root.identity
    }

    pub(in crate::hashline) fn observe(
        root: &NativeRoot,
        model_path: &str,
    ) -> Result<Observed, FunctionCallError> {
        let path = resolve(root, model_path)?;
        observe_resolved(&path)
    }

    pub(in crate::hashline) fn ensure_missing(
        root: &NativeRoot,
        model_path: &str,
    ) -> Result<(), FunctionCallError> {
        let path = resolve(root, model_path)?;
        match open_entry(&path, OFlags::RDONLY) {
            Ok(_) => model_error(format!("Hashline destination {model_path} already exists")),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
            Err(error) => Err(io_error("inspect transaction destination", error)),
        }
    }

    pub(in crate::hashline) fn lock_paths(
        root: &NativeRoot,
        prepared: &[PreparedMutation],
    ) -> Result<TransactionLease, FunctionCallError> {
        let mut parents = BTreeMap::<DirectoryIdentity, Arc<File>>::new();
        for model_path in mutation_paths(prepared) {
            let path = resolve(root, model_path)?;
            parents
                .entry(path.parent_identity)
                .or_insert_with(|| Arc::clone(&path.parent));
        }
        let covered_parents = parents.keys().copied().collect();
        let mut locks = Vec::with_capacity(parents.len());
        for parent in parents.into_values() {
            let lock = File::from(
                fs::openat(
                    &parent,
                    ".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| io_error("open transaction parent lease", error))?,
            );
            fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
                FunctionCallError::RespondToModel(format!(
                    "transaction conflict: another commit owns a participating directory: {error}"
                ))
            })?;
            locks.push(lock);
        }
        for model_path in mutation_paths(prepared) {
            verify_path_parent(&resolve(root, model_path)?)?;
        }
        Ok(TransactionLease {
            root_identity: root.identity.clone(),
            covered_parents,
            _locks: locks,
        })
    }

    pub(in crate::hashline) fn write_journal(
        root: &NativeRoot,
        transaction_id: &str,
        prepared: &[PreparedMutation],
    ) -> Result<JournalHandle, FunctionCallError> {
        let directory = open_journal_directory(root, true)?.ok_or_else(|| {
            FunctionCallError::RespondToModel("transaction storage was not created".to_owned())
        })?;
        let journal = Journal {
            version: JOURNAL_VERSION,
            transaction_id: transaction_id.to_owned(),
            applied: 0,
            mutations: prepared.iter().map(journal_mutation).collect(),
        };
        let name = format!("{transaction_id}.json");
        let file = publish_journal(&directory, &name, &journal, "prepared")?;
        Ok(JournalHandle {
            directory,
            file,
            name,
            journal,
        })
    }

    pub(in crate::hashline) fn apply_prepared(
        root: &NativeRoot,
        lease: &TransactionLease,
        prepared: &[PreparedMutation],
        journal: &mut JournalHandle,
    ) -> Result<(), FunctionCallError> {
        require_lease(root, lease)?;
        for (index, mutation) in prepared.iter().enumerate() {
            apply_one(root, lease, mutation, index)?;
            fault_point(&format!("after-mutation-{index}"));
            journal.journal.applied = index + 1;
            journal.file = publish_journal(
                &journal.directory,
                &journal.name,
                &journal.journal,
                &format!("progress-{index}"),
            )?;
        }
        Ok(())
    }

    pub(in crate::hashline) fn remove_journal(
        root: &NativeRoot,
        journal: JournalHandle,
    ) -> Result<(), FunctionCallError> {
        fault_point("before-journal-remove");
        fs::unlinkat(&journal.directory, journal.name.as_str(), AtFlags::empty())
            .map_err(|error| io_error("remove transaction journal", error))?;
        fault_point("after-journal-remove");
        journal
            .directory
            .sync_all()
            .map_err(|error| io_error("sync transaction storage", error))?;
        fault_point("after-journal-remove-dir-sync");
        drop(journal);
        cleanup_journal_directories(root)
    }

    pub(in crate::hashline) fn recover_pending(root: &NativeRoot) -> Result<(), FunctionCallError> {
        let Some(directory) = open_journal_directory(root, false)? else {
            return Ok(());
        };
        let mut names = directory_entries(&directory)?;
        names.sort();
        if names.len() > MAX_MUTATIONS {
            return model_error("transaction recovery storage exceeds the journal count limit");
        }
        for name in names {
            if name.strip_suffix(".pending").is_some() || name.strip_suffix(".next").is_some() {
                cleanup_unpublished(&directory, &name)?;
                continue;
            }
            if name.strip_suffix(".json").is_none() {
                return model_error("transaction storage contains an unrecognized artifact");
            }
            let file = open_named(&directory, &name, OFlags::RDWR)?;
            match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => continue,
                Err(error) => return Err(io_error("lock recovery journal", error)),
            }
            let journal = read_journal(&file)?;
            validate_journal_name(&name, &journal)?;
            let prepared = prepared_from_journal(&journal)?;
            let lease = lock_paths(root, &prepared)?;
            for (index, mutation) in prepared.iter().enumerate() {
                cleanup_staged_mutation(root, &lease, mutation, index)?;
            }
            for mutation in prepared.iter().rev() {
                recover_one(root, &lease, mutation)?;
            }
            fs::unlinkat(&directory, name.as_str(), AtFlags::empty())
                .map_err(|error| io_error("remove recovered journal", error))?;
            directory
                .sync_all()
                .map_err(|error| io_error("sync recovered journal directory", error))?;
        }
        cleanup_journal_directories(root)
    }

    fn resolve(root: &NativeRoot, model_path: &str) -> Result<NativePath, FunctionCallError> {
        validate_relative(model_path)?;
        if model_path == STATE_DIRECTORY
            || model_path.starts_with(&format!("{STATE_DIRECTORY}/{TRANSACTION_DIRECTORY}"))
        {
            return model_error(
                "Hashline transaction paths cannot target internal recovery storage",
            );
        }
        let path = Path::new(model_path);
        let name = path.file_name().ok_or_else(|| {
            FunctionCallError::RespondToModel("transaction path has no file name".to_owned())
        })?;
        let parent_relative = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = open_parent(&root.directory, parent_relative)?;
        ensure_supported_directory(&parent)?;
        let parent_identity = directory_identity(&parent)?;
        Ok(NativePath {
            root: Arc::clone(&root.directory),
            parent: Arc::new(parent),
            parent_relative: parent_relative.to_path_buf(),
            parent_identity,
            name: name.to_os_string(),
            model_path: model_path.to_owned(),
        })
    }

    fn validate_relative(model_path: &str) -> Result<(), FunctionCallError> {
        let path = Path::new(model_path);
        if model_path.is_empty() || path.is_absolute() {
            return model_error("Hashline paths must be non-empty and workspace-relative");
        }
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return model_error(format!(
                "Hashline path {model_path} contains an invalid component"
            ));
        }
        Ok(())
    }

    fn open_parent(root: &File, relative: &Path) -> Result<File, FunctionCallError> {
        let relative = if relative == Path::new("") {
            Path::new(".")
        } else {
            relative
        };
        let fd = fs::openat2(
            root,
            relative,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|error| io_error("resolve transaction parent", error))?;
        Ok(File::from(fd))
    }

    fn open_entry(path: &NativePath, access: OFlags) -> Result<File, rustix::io::Errno> {
        fs::openat2(
            &path.parent,
            &path.name,
            access | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map(File::from)
    }

    fn open_named(directory: &File, name: &str, access: OFlags) -> Result<File, FunctionCallError> {
        fs::openat2(
            directory,
            name,
            access | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map(File::from)
        .map_err(|error| io_error("open transaction artifact", error))
    }

    fn verify_path_parent(path: &NativePath) -> Result<(), FunctionCallError> {
        let reopened = open_parent(&path.root, &path.parent_relative)?;
        if directory_identity(&reopened)? != path.parent_identity {
            return model_error(format!(
                "transaction path {} changed parent identity while leased",
                path.model_path
            ));
        }
        Ok(())
    }

    fn observe_resolved(path: &NativePath) -> Result<Observed, FunctionCallError> {
        verify_path_parent(path)?;
        let mut file = open_entry(path, OFlags::RDONLY)
            .map_err(|error| io_error("open transaction file", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction file", error))?;
        if !metadata.is_file() {
            return model_error(format!(
                "Hashline path {} is not a regular file",
                path.model_path
            ));
        }
        if metadata.nlink() != 1 {
            return model_error(format!(
                "Hashline path {} has multiple hard links",
                path.model_path
            ));
        }
        if metadata.len() > super::super::MAX_FILE_BYTES {
            return model_error("Hashline file exceeds the configured byte limit");
        }
        let mut bytes = Vec::with_capacity(bounded_capacity(metadata.len())?);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction file", error))?;
        let after = file
            .metadata()
            .map_err(|error| io_error("reinspect transaction file", error))?;
        if !same_observation(&metadata, &after) {
            return model_error(format!(
                "Hashline path {} changed while it was observed; reread and retry",
                path.model_path
            ));
        }
        if bytes.contains(&0) {
            return model_error(format!(
                "Hashline path {} contains NUL/binary content",
                path.model_path
            ));
        }
        let text = String::from_utf8(bytes.clone()).map_err(|_| {
            FunctionCallError::RespondToModel(format!(
                "Hashline path {} is not valid UTF-8",
                path.model_path
            ))
        })?;
        Ok(Observed { bytes, text })
    }

    fn require_lease(root: &NativeRoot, lease: &TransactionLease) -> Result<(), FunctionCallError> {
        if lease.root_identity != root.identity {
            return model_error("transaction lease belongs to a different root");
        }
        Ok(())
    }

    fn require_path_lease(
        lease: &TransactionLease,
        path: &NativePath,
    ) -> Result<(), FunctionCallError> {
        if !lease.covered_parents.contains(&path.parent_identity) {
            return model_error(format!(
                "transaction path {} is not covered by the retained lease",
                path.model_path
            ));
        }
        verify_path_parent(path)
    }

    fn apply_one(
        root: &NativeRoot,
        lease: &TransactionLease,
        mutation: &PreparedMutation,
        index: usize,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write {
                path,
                before,
                after,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match before {
                    Some(expected) => require_bytes(&target, expected)?,
                    None => require_absent(&target)?,
                }
                atomic_write(&target, after, &format!("mutation-{index}"))
            }
            PreparedMutation::Delete { path, before } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                require_bytes(&target, before)?;
                fs::unlinkat(&target.parent, &target.name, AtFlags::empty())
                    .map_err(|error| io_error("delete transaction file", error))?;
                fault_point(&format!("mutation-{index}-after-unlink"));
                target
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction parent", error))?;
                fault_point(&format!("mutation-{index}-after-parent-sync"));
                Ok(())
            }
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
            } => {
                let source_path = resolve(root, source)?;
                let destination_path = resolve(root, destination)?;
                require_path_lease(lease, &source_path)?;
                require_path_lease(lease, &destination_path)?;
                require_bytes(&source_path, before)?;
                require_absent(&destination_path)?;
                atomic_write(
                    &destination_path,
                    after,
                    &format!("mutation-{index}-destination"),
                )?;
                fs::unlinkat(&source_path.parent, &source_path.name, AtFlags::empty())
                    .map_err(|error| io_error("remove transaction move source", error))?;
                fault_point(&format!("mutation-{index}-after-source-unlink"));
                source_path
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction move source parent", error))?;
                fault_point(&format!("mutation-{index}-after-source-parent-sync"));
                Ok(())
            }
        }
    }

    fn recover_one(
        root: &NativeRoot,
        lease: &TransactionLease,
        mutation: &PreparedMutation,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write {
                path,
                before: Some(before),
                after,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_bytes(&target)? {
                    Some(current) if current == *before => Ok(()),
                    Some(current) if current == *after => {
                        atomic_write(&target, before, "recover-write")
                    }
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Write {
                path,
                before: None,
                after,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_bytes(&target)? {
                    None => Ok(()),
                    Some(current) if current == *after => {
                        fs::unlinkat(&target.parent, &target.name, AtFlags::empty())
                            .map_err(|error| io_error("recover transaction create", error))?;
                        target
                            .parent
                            .sync_all()
                            .map_err(|error| io_error("sync recovered create parent", error))
                    }
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Delete { path, before } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_bytes(&target)? {
                    None => atomic_write(&target, before, "recover-delete"),
                    Some(current) if current == *before => Ok(()),
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
            } => {
                let source_path = resolve(root, source)?;
                let destination_path = resolve(root, destination)?;
                require_path_lease(lease, &source_path)?;
                require_path_lease(lease, &destination_path)?;
                let source_bytes = entry_bytes(&source_path)?;
                let destination_bytes = entry_bytes(&destination_path)?;
                match (source_bytes, destination_bytes) {
                    (Some(source), None) if source == *before => Ok(()),
                    (None, Some(destination_bytes)) if destination_bytes == *after => {
                        fs::unlinkat(
                            &destination_path.parent,
                            &destination_path.name,
                            AtFlags::empty(),
                        )
                        .map_err(|error| io_error("recover move destination", error))?;
                        destination_path
                            .parent
                            .sync_all()
                            .map_err(|error| io_error("sync recovered move destination", error))?;
                        atomic_write(&source_path, before, "recover-move")
                    }
                    (Some(source), Some(destination_bytes))
                        if source == *before && destination_bytes == *after =>
                    {
                        fs::unlinkat(
                            &destination_path.parent,
                            &destination_path.name,
                            AtFlags::empty(),
                        )
                        .map_err(|error| io_error("recover partial move destination", error))?;
                        destination_path
                            .parent
                            .sync_all()
                            .map_err(|error| io_error("sync recovered partial move", error))
                    }
                    _ => recovery_conflict(source),
                }
            }
        }
    }

    fn atomic_write(
        target: &NativePath,
        contents: &[u8],
        transition: &str,
    ) -> Result<(), FunctionCallError> {
        verify_path_parent(target)?;
        let temporary = temporary_name(&target.model_path, transition);
        let fd = fs::openat(
            &target.parent,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error("stage transaction file", error))?;
        let mut file = File::from(fd);
        if let Ok(existing) = open_entry(target, OFlags::RDONLY) {
            let permissions = existing
                .metadata()
                .map_err(|error| io_error("inspect transaction permissions", error))?
                .permissions();
            file.set_permissions(permissions)
                .map_err(|error| io_error("preserve transaction permissions", error))?;
        }
        let result = (|| {
            file.write_all(contents)
                .map_err(|error| io_error("write transaction stage", error))?;
            file.sync_all()
                .map_err(|error| io_error("sync transaction stage", error))?;
            fault_point(&format!("{transition}-after-stage-sync"));
            fs::renameat(
                &target.parent,
                temporary.as_str(),
                &target.parent,
                &target.name,
            )
            .map_err(|error| io_error("publish transaction file", error))?;
            fault_point(&format!("{transition}-after-rename"));
            target
                .parent
                .sync_all()
                .map_err(|error| io_error("sync transaction parent", error))?;
            fault_point(&format!("{transition}-after-parent-sync"));
            require_bytes(target, contents)
        })();
        if result.is_err() {
            let _ = fs::unlinkat(&target.parent, temporary.as_str(), AtFlags::empty());
        }
        result
    }

    fn cleanup_staged_mutation(
        root: &NativeRoot,
        lease: &TransactionLease,
        mutation: &PreparedMutation,
        index: usize,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write { path, after, .. } => {
                cleanup_staged_file(root, lease, path, after, &format!("mutation-{index}"))
            }
            PreparedMutation::Move {
                destination, after, ..
            } => cleanup_staged_file(
                root,
                lease,
                destination,
                after,
                &format!("mutation-{index}-destination"),
            ),
            PreparedMutation::Delete { .. } => Ok(()),
        }
    }

    fn cleanup_staged_file(
        root: &NativeRoot,
        lease: &TransactionLease,
        model_path: &str,
        expected: &[u8],
        transition: &str,
    ) -> Result<(), FunctionCallError> {
        let target = resolve(root, model_path)?;
        require_path_lease(lease, &target)?;
        let name = temporary_name(model_path, transition);
        let mut file = match fs::openat2(
            &target.parent,
            name.as_str(),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        ) {
            Ok(fd) => File::from(fd),
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
            Err(error) => return Err(io_error("open interrupted transaction stage", error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect interrupted transaction stage", error))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > super::super::MAX_FILE_BYTES
        {
            return model_error("interrupted transaction stage has invalid evidence");
        }
        let mut bytes = Vec::with_capacity(bounded_capacity(metadata.len())?);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read interrupted transaction stage", error))?;
        if bytes != expected {
            return model_error("interrupted transaction stage does not match retained evidence");
        }
        fs::unlinkat(&target.parent, name.as_str(), AtFlags::empty())
            .map_err(|error| io_error("remove interrupted transaction stage", error))?;
        target
            .parent
            .sync_all()
            .map_err(|error| io_error("sync interrupted transaction stage cleanup", error))
    }

    fn temporary_name(model_path: &str, transition: &str) -> String {
        format!(
            ".nanocodex-hashline-{}.tmp",
            exact_digest(format!("{model_path}:{transition}").as_bytes())
        )
    }

    fn require_bytes(path: &NativePath, expected: &[u8]) -> Result<(), FunctionCallError> {
        match entry_bytes(path)? {
            Some(bytes) if bytes == expected => Ok(()),
            _ => model_error(format!(
                "transaction evidence changed for {}; reread and rebuild the plan",
                path.model_path
            )),
        }
    }

    fn require_absent(path: &NativePath) -> Result<(), FunctionCallError> {
        if entry_bytes(path)?.is_none() {
            Ok(())
        } else {
            model_error(format!(
                "transaction destination {} is no longer absent",
                path.model_path
            ))
        }
    }

    fn entry_bytes(path: &NativePath) -> Result<Option<Vec<u8>>, FunctionCallError> {
        verify_path_parent(path)?;
        let mut file = match open_entry(path, OFlags::RDONLY) {
            Ok(file) => file,
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) => return Err(io_error("open transaction evidence", error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction evidence", error))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > super::super::MAX_FILE_BYTES
        {
            return model_error(format!(
                "transaction evidence for {} is invalid",
                path.model_path
            ));
        }
        let mut bytes = Vec::with_capacity(bounded_capacity(metadata.len())?);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction evidence", error))?;
        Ok(Some(bytes))
    }

    fn mutation_paths(prepared: &[PreparedMutation]) -> impl Iterator<Item = &str> {
        prepared
            .iter()
            .flat_map(|mutation| match mutation {
                PreparedMutation::Write { path, .. } | PreparedMutation::Delete { path, .. } => {
                    [Some(path.as_str()), None]
                }
                PreparedMutation::Move {
                    source,
                    destination,
                    ..
                } => [Some(source.as_str()), Some(destination.as_str())],
            })
            .flatten()
    }

    fn journal_mutation(mutation: &PreparedMutation) -> JournalMutation {
        match mutation {
            PreparedMutation::Write {
                path,
                before,
                after,
            } => JournalMutation {
                kind: "write".to_owned(),
                path: path.clone(),
                destination: None,
                before: before.as_deref().map(encode_bytes),
                after: Some(encode_bytes(after)),
            },
            PreparedMutation::Delete { path, before } => JournalMutation {
                kind: "delete".to_owned(),
                path: path.clone(),
                destination: None,
                before: Some(encode_bytes(before)),
                after: None,
            },
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
            } => JournalMutation {
                kind: "move".to_owned(),
                path: source.clone(),
                destination: Some(destination.clone()),
                before: Some(encode_bytes(before)),
                after: Some(encode_bytes(after)),
            },
        }
    }

    fn prepared_from_journal(
        journal: &Journal,
    ) -> Result<Vec<PreparedMutation>, FunctionCallError> {
        journal
            .mutations
            .iter()
            .map(
                |mutation| match (mutation.kind.as_str(), mutation.before.as_deref()) {
                    ("write", before) => {
                        let before = before.map(decode_bytes).transpose()?;
                        let after = mutation
                            .after
                            .as_deref()
                            .ok_or_else(|| {
                                FunctionCallError::RespondToModel(
                                    "write journal lacks after evidence".to_owned(),
                                )
                            })
                            .and_then(decode_bytes)?;
                        Ok(PreparedMutation::Write {
                            path: mutation.path.clone(),
                            before,
                            after,
                        })
                    }
                    ("delete", Some(before)) => Ok(PreparedMutation::Delete {
                        path: mutation.path.clone(),
                        before: decode_bytes(before)?,
                    }),
                    ("move", Some(before)) => {
                        let before = decode_bytes(before)?;
                        let after = mutation
                            .after
                            .as_deref()
                            .ok_or_else(|| {
                                FunctionCallError::RespondToModel(
                                    "move journal lacks after evidence".to_owned(),
                                )
                            })
                            .and_then(decode_bytes)?;
                        let destination = mutation.destination.clone().ok_or_else(|| {
                            FunctionCallError::RespondToModel(
                                "move journal lacks destination".to_owned(),
                            )
                        })?;
                        Ok(PreparedMutation::Move {
                            source: mutation.path.clone(),
                            destination,
                            before,
                            after,
                        })
                    }
                    _ => model_error("transaction journal contains an invalid mutation"),
                },
            )
            .collect()
    }

    fn publish_journal(
        directory: &File,
        name: &str,
        journal: &Journal,
        transition: &str,
    ) -> Result<File, FunctionCallError> {
        let temporary = format!("{}.{}.next", journal.transaction_id, transition);
        let _ = fs::unlinkat(directory, temporary.as_str(), AtFlags::empty());
        let fd = fs::openat(
            directory,
            temporary.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error("create transaction journal", error))?;
        let mut file = File::from(fd);
        fs::flock(&file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|error| io_error("lock transaction journal", error))?;
        let bytes = serde_json::to_vec(journal)
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        if bytes.len() > MAX_TOTAL_BYTES.saturating_mul(3) {
            return model_error("transaction journal exceeds its byte limit");
        }
        file.write_all(&bytes)
            .map_err(|error| io_error("write transaction journal", error))?;
        file.sync_all()
            .map_err(|error| io_error("sync transaction journal", error))?;
        fault_point(&format!("{transition}-journal-file-sync"));
        fs::renameat(directory, temporary.as_str(), directory, name)
            .map_err(|error| io_error("publish transaction journal", error))?;
        fault_point(&format!("{transition}-journal-publish"));
        directory
            .sync_all()
            .map_err(|error| io_error("sync transaction journal directory", error))?;
        fault_point(&format!("{transition}-journal-dir-sync"));
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io_error("rewind transaction journal", error))?;
        Ok(file)
    }

    fn read_journal(file: &File) -> Result<Journal, FunctionCallError> {
        let mut reader = file
            .try_clone()
            .map_err(|error| io_error("clone transaction journal", error))?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error("rewind transaction journal", error))?;
        let mut bytes = Vec::new();
        reader
            .take((MAX_TOTAL_BYTES.saturating_mul(3) + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction journal", error))?;
        if bytes.len() > MAX_TOTAL_BYTES.saturating_mul(3) {
            return model_error("transaction journal exceeds its byte limit");
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            FunctionCallError::RespondToModel(format!("invalid transaction journal: {error}"))
        })
    }

    fn validate_journal_name(name: &str, journal: &Journal) -> Result<(), FunctionCallError> {
        if journal.version != JOURNAL_VERSION
            || name != format!("{}.json", journal.transaction_id)
            || journal.mutations.is_empty()
            || journal.mutations.len() > MAX_MUTATIONS
            || journal.applied > journal.mutations.len()
        {
            return model_error("transaction journal identity or bounds are invalid");
        }
        Ok(())
    }

    fn open_journal_directory(
        root: &NativeRoot,
        create: bool,
    ) -> Result<Option<Arc<File>>, FunctionCallError> {
        let state = open_or_create_directory(&root.directory, STATE_DIRECTORY, create)?;
        let Some(state) = state else { return Ok(None) };
        let transactions = open_or_create_directory(&state, TRANSACTION_DIRECTORY, create)?;
        Ok(transactions.map(Arc::new))
    }

    fn open_or_create_directory(
        parent: &File,
        name: &str,
        create: bool,
    ) -> Result<Option<File>, FunctionCallError> {
        match fs::openat2(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        ) {
            Ok(fd) => {
                let file = File::from(fd);
                ensure_supported_directory(&file)?;
                Ok(Some(file))
            }
            Err(error) if error == rustix::io::Errno::NOENT && create => {
                fs::mkdirat(parent, name, Mode::from_raw_mode(0o700))
                    .map_err(|error| io_error("create transaction storage directory", error))?;
                parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction storage parent", error))?;
                open_or_create_directory(parent, name, false)
            }
            Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
            Err(error) => Err(io_error("open transaction storage directory", error)),
        }
    }

    fn directory_entries(directory: &File) -> Result<Vec<String>, FunctionCallError> {
        let stream = Dir::read_from(directory)
            .map_err(|error| io_error("scan transaction storage", error))?;
        let mut names = Vec::new();
        for entry in stream {
            let entry = entry.map_err(|error| io_error("scan transaction storage", error))?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            names.push(String::from_utf8(name.to_bytes().to_vec()).map_err(|_| {
                FunctionCallError::RespondToModel(
                    "transaction storage contains a non-UTF-8 artifact".to_owned(),
                )
            })?);
        }
        Ok(names)
    }

    fn cleanup_unpublished(directory: &File, name: &str) -> Result<(), FunctionCallError> {
        let file = open_named(directory, name, OFlags::RDWR)?;
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                fs::unlinkat(directory, name, AtFlags::empty())
                    .map_err(|error| io_error("remove unpublished transaction artifact", error))?;
                directory
                    .sync_all()
                    .map_err(|error| io_error("sync transaction storage cleanup", error))
            }
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(()),
            Err(error) => Err(io_error("lock unpublished transaction artifact", error)),
        }
    }

    fn cleanup_journal_directories(root: &NativeRoot) -> Result<(), FunctionCallError> {
        let Some(directory) = open_journal_directory(root, false)? else {
            return Ok(());
        };
        if !directory_entries(&directory)?.is_empty() {
            return Ok(());
        }
        fs::unlinkat(
            &root.directory,
            format!("{STATE_DIRECTORY}/{TRANSACTION_DIRECTORY}"),
            AtFlags::REMOVEDIR,
        )
        .map_err(|error| io_error("remove empty transaction storage", error))?;
        let state = open_or_create_directory(&root.directory, STATE_DIRECTORY, false)?;
        if let Some(state) = state
            && directory_entries(&state)?.is_empty()
        {
            fs::unlinkat(&root.directory, STATE_DIRECTORY, AtFlags::REMOVEDIR)
                .map_err(|error| io_error("remove empty transaction state", error))?;
        }
        root.directory
            .sync_all()
            .map_err(|error| io_error("sync transaction root cleanup", error))
    }

    fn ensure_supported_directory(directory: &File) -> Result<(), FunctionCallError> {
        let filesystem = fs::fstatfs(directory)
            .map_err(|error| io_error("inspect transaction filesystem", error))?;
        let filesystem_type = filesystem.f_type as i64;
        if filesystem_type == TMPFS_MAGIC {
            return Ok(());
        }
        if filesystem_type != EXT_SUPER_MAGIC {
            return model_error(format!(
                "unsupported: durable Hashline transactions require proven Linux ext-family or tmpfs semantics; found {filesystem_type:#x}"
            ));
        }
        let flags = fs::ioctl_getflags(directory)
            .map_err(|error| io_error("inspect transaction directory flags", error))?;
        if flags.intersects(IFlags::from_bits_retain(CASEFOLD_FLAG)) {
            return model_error(
                "unsupported: durable Hashline transactions require case-sensitive directory lookup",
            );
        }
        Ok(())
    }

    fn directory_identity(file: &File) -> Result<DirectoryIdentity, FunctionCallError> {
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction directory identity", error))?;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn identity_bytes(metadata: &std::fs::Metadata) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&metadata.dev().to_le_bytes());
        bytes.extend_from_slice(&metadata.ino().to_le_bytes());
        bytes
    }

    fn same_observation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mode() == after.mode()
            && before.nlink() == after.nlink()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }

    fn bounded_capacity(length: u64) -> Result<usize, FunctionCallError> {
        usize::try_from(length).map_err(|_| {
            FunctionCallError::RespondToModel(
                "transaction file length does not fit this platform".to_owned(),
            )
        })
    }

    fn recovery_conflict<T>(path: &str) -> Result<T, FunctionCallError> {
        model_error(format!(
            "transaction recovery evidence for {path} is neither the retained before nor after state; manual recovery is required"
        ))
    }

    fn io_error(operation: &str, error: impl std::fmt::Display) -> FunctionCallError {
        FunctionCallError::RespondToModel(format!("{operation}: {error}"))
    }

    #[cfg(test)]
    fn fault_point(name: &str) {
        if std::env::var("NANOCODEX_HASHLINE_FAULT").as_deref() == Ok(name) {
            std::process::abort();
        }
    }

    #[cfg(not(test))]
    fn fault_point(_name: &str) {}
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::path::Path;

    use super::super::{FunctionCallError, Observed, PreparedMutation, model_error};

    #[derive(Debug)]
    pub(in crate::hashline) struct NativeRoot;
    #[derive(Debug)]
    pub(in crate::hashline) struct TransactionLease;
    #[derive(Debug)]
    pub(in crate::hashline) struct JournalHandle;

    pub(in crate::hashline) fn open_root(
        _workspace: &Path,
        _root_name: &str,
    ) -> Result<NativeRoot, FunctionCallError> {
        model_error(
            "unsupported: durable Hashline transactions currently require Linux ext-family or tmpfs filesystem semantics",
        )
    }

    pub(in crate::hashline) fn root_identity(_root: &NativeRoot) -> &[u8] {
        &[]
    }
    pub(in crate::hashline) fn observe(
        _root: &NativeRoot,
        _path: &str,
    ) -> Result<Observed, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn ensure_missing(
        _root: &NativeRoot,
        _path: &str,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn recover_pending(
        _root: &NativeRoot,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn lock_paths(
        _root: &NativeRoot,
        _prepared: &[PreparedMutation],
    ) -> Result<TransactionLease, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn write_journal(
        _root: &NativeRoot,
        _id: &str,
        _prepared: &[PreparedMutation],
    ) -> Result<JournalHandle, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn apply_prepared(
        _root: &NativeRoot,
        _lease: &TransactionLease,
        _prepared: &[PreparedMutation],
        _journal: &mut JournalHandle,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn remove_journal(
        _root: &NativeRoot,
        _journal: JournalHandle,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
}

pub(super) use platform::*;
