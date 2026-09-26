//! Private launch trees built only from admitted bytes. They never reread an
//! original package path. A supervisor retains the group lease until both
//! process reaping and native consumer cleanup have completed.
use crate::{
    error::{ErrorCode, ProtocolError, Result},
    prepare::{digest, FileKind, PluginEntry, PreparedDeployment, PreparedFile},
};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

struct Tree {
    root: PathBuf,
    removed: bool,
}
impl Tree {
    fn create() -> Result<Self> {
        let parent =
            fs::canonicalize(std::env::temp_dir()).map_err(|e| unavailable(e.to_string()))?;
        for _ in 0..8 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|e| unavailable(e.to_string()))?;
            let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let root = parent.join(format!("rutis-protocol-snapshot-{name}"));
            match DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => {
                    return Ok(Self {
                        root,
                        removed: false,
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(unavailable(e.to_string())),
            }
        }
        Err(unavailable(
            "could not allocate a private snapshot directory",
        ))
    }
    fn cleanup(mut self) -> Result<()> {
        fs::remove_dir_all(&self.root).map_err(|e| unavailable(e.to_string()))?;
        self.removed = true;
        Ok(())
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        if !self.removed {
            if let Err(error) = fs::remove_dir_all(&self.root) {
                eprintln!("[rutis-protocol] snapshot cleanup failed: {error}");
            }
        }
    }
}

#[derive(Clone)]
pub struct SnapshotMember {
    // A copied member descriptor also keeps all its canonical dependencies alive.
    _tree: Arc<Tree>,
    root: PathBuf,
    entry: Option<PathBuf>,
}
impl SnapshotMember {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn entry(&self) -> Option<&Path> {
        self.entry.as_deref()
    }
}

/// Cloning a group retains the entire private tree, including shared files.
/// Returned paths are valid only while at least one snapshot/group lease lives.
#[derive(Clone)]
pub struct SnapshotGroup {
    tree: Arc<Tree>,
    environment: PathBuf,
    executable: PathBuf,
    runner: Option<PathBuf>,
    members: BTreeMap<String, SnapshotMember>,
    code_sha256: String,
}
impl SnapshotGroup {
    pub fn code_sha256(&self) -> &str {
        &self.code_sha256
    }
    pub fn environment(&self) -> &Path {
        &self.environment
    }
    pub fn members(&self) -> &BTreeMap<String, SnapshotMember> {
        &self.members
    }
    pub fn argv(&self) -> Vec<PathBuf> {
        let mut args = vec![self.executable.clone()];
        if let Some(runner) = &self.runner {
            args.push(runner.clone());
        }
        args
    }
    pub fn snapshot_root(&self) -> &Path {
        &self.tree.root
    }
}

pub struct Snapshot {
    tree: Arc<Tree>,
    groups: BTreeMap<String, SnapshotGroup>,
}
impl Snapshot {
    /// The original package can change or disappear after admission without
    /// changing this tree. This is artifact pinning, not a same-uid OS sandbox.
    pub fn materialize(plan: &PreparedDeployment) -> Result<Self> {
        let tree = Arc::new(Tree::create()?);
        let mut artifacts = BTreeMap::new();
        let mut groups = BTreeMap::new();
        for (name, group) in plan.groups() {
            let root = tree.root.join("groups").join(digest(name.as_bytes()));
            let environment = root.join("environment");
            mkdir(&environment)?;
            let first = group
                .members()
                .iter()
                .next()
                .expect("prepared groups are nonempty");
            let package = plan.instances()[first].package();
            for spec in package
                .manifest()
                .files
                .iter()
                .filter(|s| s.kind == FileKind::Dependency)
            {
                let file = package.file(&spec.path)?;
                if file.canonical_name() == spec.path {
                    let artifact = store(&tree.root, &mut artifacts, file, file.is_executable())?;
                    link(&artifact, &environment.join(&spec.path), false)?;
                } else {
                    link(
                        &environment.join(file.canonical_name()),
                        &environment.join(&spec.path),
                        true,
                    )?;
                }
            }
            let mut packages: BTreeMap<String, PathBuf> = BTreeMap::new();
            let mut members = BTreeMap::new();
            for member in group.members() {
                let package = plan.instances()[member].package();
                let id = package.snapshot_sha256().to_owned();
                let package_root = if let Some(root) = packages.get(&id) {
                    root.clone()
                } else {
                    let package_root = root
                        .join("packages")
                        .join(&id)
                        .join(&package.manifest().version);
                    mkdir(&package_root)?;
                    write(
                        &package_root.join(crate::prepare::MANIFEST),
                        package.manifest_bytes(),
                        false,
                    )?;
                    for spec in &package.manifest().files {
                        let target = package_root.join(&spec.path);
                        if spec.kind == FileKind::Dependency {
                            // Node resolves these files to one canonical path,
                            // keeping Cordis/SDK and their dependency modules singletons.
                            link(&environment.join(&spec.path), &target, true)?;
                        } else {
                            let file = package.file(&spec.path)?;
                            if file.canonical_name() != spec.path {
                                let canonical = package
                                    .manifest()
                                    .files
                                    .iter()
                                    .find(|s| s.path == file.canonical_name())
                                    .expect("complete inventory contains the canonical file");
                                let source = if canonical.kind == FileKind::Dependency {
                                    environment.join(&canonical.path)
                                } else {
                                    package_root.join(&canonical.path)
                                };
                                link(&source, &target, true)?;
                                continue;
                            }
                            let artifact =
                                store(&tree.root, &mut artifacts, file, file.is_executable())?;
                            link(&artifact, &target, false)?;
                        }
                    }
                    packages.insert(id, package_root.clone());
                    package_root
                };
                let entry = match &package.manifest().plugin {
                    PluginEntry::Node { entry } => Some(package_root.join(entry)),
                    PluginEntry::Rust { .. } => None,
                };
                members.insert(
                    member.clone(),
                    SnapshotMember {
                        _tree: tree.clone(),
                        root: package_root,
                        entry,
                    },
                );
            }
            let first_root = &members[first].root;
            let executable = first_root.join(&package.manifest().runtime.executable);
            let runner = package
                .manifest()
                .runtime
                .runner
                .as_ref()
                .map(|path| first_root.join(path));
            groups.insert(
                name.clone(),
                SnapshotGroup {
                    tree: tree.clone(),
                    environment,
                    executable,
                    runner,
                    members,
                    code_sha256: group.code_sha256().into(),
                },
            );
        }
        Ok(Self { tree, groups })
    }
    pub fn groups(&self) -> &BTreeMap<String, SnapshotGroup> {
        &self.groups
    }
    pub fn root(&self) -> &Path {
        &self.tree.root
    }

    /// Refuses to remove paths still leased by a group. The supervisor must
    /// first reap its process/descendants and finish consumer cleanup.
    pub fn cleanup(self) -> Result<()> {
        drop(self.groups);
        Arc::try_unwrap(self.tree)
            .map_err(|_| unavailable("snapshot is still leased by a runtime group"))?
            .cleanup()
    }
}

fn mkdir(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| unavailable(e.to_string()))
}
fn write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| unavailable(e.to_string()))?;
    file.write_all(bytes)
        .map_err(|e| unavailable(e.to_string()))?;
    file.set_permissions(fs::Permissions::from_mode(if executable {
        0o555
    } else {
        0o444
    }))
    .map_err(|e| unavailable(e.to_string()))
}
fn link(source: &Path, target: &Path, symbolic: bool) -> Result<()> {
    mkdir(target.parent().expect("snapshot artifact has a parent"))?;
    if symbolic {
        symlink(source, target)
    } else {
        fs::hard_link(source, target)
    }
    .map_err(|e| unavailable(e.to_string()))
}
fn store(
    root: &Path,
    artifacts: &mut BTreeMap<(String, bool), PathBuf>,
    file: &PreparedFile,
    executable: bool,
) -> Result<PathBuf> {
    let key = (file.sha256().to_owned(), executable);
    if let Some(path) = artifacts.get(&key) {
        return Ok(path.clone());
    }
    let path = root.join("artifacts").join(format!(
        "{}.{}",
        file.sha256(),
        if executable { "exec" } else { "data" }
    ));
    mkdir(path.parent().expect("artifact cache has a parent"))?;
    write(&path, file.bytes(), executable)?;
    artifacts.insert(key, path.clone());
    Ok(path)
}
fn unavailable(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, "snapshot", message)
}
