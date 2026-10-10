use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use globset::{Glob, GlobSet, GlobSetBuilder};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Refusing a symlink for private storage"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.contains('\0'),
        "Invalid empty path"
    );
    ensure!(
        Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "Unsafe relative path: {path}"
    );
    ensure!(
        !path.split('/').any(|c| c.starts_with(".peedrive")),
        "Reserved Peedrive path"
    );
    Ok(())
}
pub fn rooted(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative(relative)?;
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "Symbolic link needs attention: {}",
                path.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}
pub fn root_identity(root: &Path) -> Result<(u64, u64)> {
    let meta = fs::symlink_metadata(root).context("Sync folder is unavailable")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Sync root must be a real directory"
    );
    Ok((meta.dev(), meta.ino()))
}
pub fn verify_root(mapping: &Mapping) -> Result<()> {
    ensure!(
        root_identity(Path::new(&mapping.local))? == (mapping.root_device, mapping.root_inode),
        "Sync folder was replaced or its disk is unavailable. Reconnect the original folder."
    );
    Ok(())
}
pub fn ignores(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p)?);
    }
    Ok(b.build()?)
}
pub fn ignored(path: &str, set: &GlobSet) -> bool {
    path.split('/').any(|c| c.starts_with(".peedrive"))
        || set.is_match(path)
        || Path::new(path).ancestors().skip(1).any(|p| set.is_match(p))
}
/// Ancestors are traversed and created as scaffolding, never deleted by a partial selection.
pub fn selected(mapping: &Mapping, path: &str, ancestors: bool) -> bool {
    mapping.selection.as_ref().is_none_or(|paths| {
        paths.iter().any(|p| {
            Path::new(path).starts_with(p) || (ancestors && Path::new(p).starts_with(path))
        })
    })
}
/// Inode identity and change times. Any content write updates ctime, which userspace cannot set.
type Stamp = (u64, u64, u64, i64, i64, i64, i64);
fn stamp(m: &fs::Metadata) -> Stamp {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
type Hashes = HashMap<(u64, u64), (Stamp, String)>;
static HASHES: Mutex<Option<Hashes>> = Mutex::new(None);
/// Files changed this recently are rehashed: coarse kernel timestamps could hide a second write.
const SETTLED_SECONDS: i64 = 2;

pub fn entry(path: &Path) -> Result<Option<Entry>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let uid = format!("{}:{}", meta.dev(), meta.ino());
    if meta.is_dir() {
        return Ok(Some(Entry {
            kind: Kind::Folder,
            version: "directory".into(),
            uid,
            size: 0,
            hash: None,
        }));
    }
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "Unsupported file type: {}. Add an ignore rule or move it out of the sync folder.",
        path.display()
    );
    let before = stamp(&meta);
    let cached = HASHES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|c| c.get(&(meta.dev(), meta.ino())).cloned());
    if let Some((known, hash)) = cached
        && known == before
    {
        return Ok(Some(file_entry(uid, meta.len(), hash)));
    }
    let mut options = fs::OpenOptions::new();
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    ensure!(
        before == stamp(&fs::symlink_metadata(path)?),
        "File changed while reading: {}",
        path.display()
    );
    let hash = format!("{:x}", hasher.finalize());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if meta.ctime() < now - SETTLED_SECONDS {
        let mut cache = HASHES.lock().unwrap();
        let cache = cache.get_or_insert_with(HashMap::new);
        if cache.len() > 500_000 {
            cache.clear();
        }
        cache.insert((meta.dev(), meta.ino()), (before, hash.clone()));
    }
    Ok(Some(file_entry(uid, meta.len(), hash)))
}
fn file_entry(uid: String, size: u64, hash: String) -> Entry {
    Entry {
        kind: Kind::File,
        version: hash.clone(),
        uid,
        size,
        hash: Some(hash),
    }
}
pub fn scan(mapping: &Mapping) -> Result<Tree> {
    verify_root(mapping)?;
    let root = Path::new(&mapping.local);
    let set = ignores(&mapping.ignores)?;
    let mut tree = Tree::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || e.path()
                    .strip_prefix(root)
                    .ok()
                    .and_then(|p| p.to_str())
                    .is_none_or(|p| !ignored(p, &set) && selected(mapping, p, true))
        });
    for item in walker {
        let item = item?;
        if item.depth() == 0 {
            continue;
        }
        let rel = item
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("A filename is not valid UTF-8; rename it before syncing")?
            .to_string();
        validate_relative(&rel)?;
        if let Some(value) = entry(item.path())? {
            tree.insert(rel, value);
        } else {
            bail!("Folder changed during scan; retry after writes finish");
        }
    }
    verify_root(mapping)?;
    Ok(tree)
}
pub fn assert_unchanged(root: &Path, path: &str, expected: &Option<Entry>) -> Result<()> {
    ensure!(
        entry(&rooted(root, path)?)? == *expected,
        "Local file changed after planning: {path}"
    );
    Ok(())
}
pub fn recovery_path(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative(relative)?;
    let base = root.join(".peedrive-recovery");
    private_dir(&base)?;
    let dir = base.join(uuid::Uuid::new_v4().to_string());
    private_dir(&dir)?;
    Ok(dir.join(
        Path::new(relative)
            .file_name()
            .context("Missing filename")?,
    ))
}
pub fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().context("Missing parent")?)?.sync_all()?;
    Ok(())
}

/// Linux atomic rename that refuses to replace a destination created concurrently.
pub fn rename_new(source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Commit a verified staged file and retain its pre-install content identity.
/// Rename preserves the inode. Re-hashing the destination here would incorrectly
/// absorb a concurrent editor's bytes into the synced baseline.
pub fn install_download(source: &Path, destination: &Path, verified: Entry) -> Result<Entry> {
    rename_new(source, destination)?;
    sync_parent(destination)?;
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn settled_files_reuse_hashes_until_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, "first").unwrap();
        let fresh = entry(&path).unwrap().unwrap();
        assert!(
            HASHES.lock().unwrap().as_ref().is_none_or(|c| {
                let m = fs::metadata(&path).unwrap();
                !c.contains_key(&(m.dev(), m.ino()))
            }),
            "Recently written files must not be cached"
        );
        std::thread::sleep(Duration::from_secs(SETTLED_SECONDS as u64 + 1));
        assert_eq!(entry(&path).unwrap().unwrap(), fresh);
        assert_eq!(entry(&path).unwrap().unwrap(), fresh, "cached");
        // Same size, rewritten in place: ctime moves, so the cache cannot hide it.
        fs::write(&path, "other").unwrap();
        assert_ne!(entry(&path).unwrap().unwrap().hash, fresh.hash);
    }
}
