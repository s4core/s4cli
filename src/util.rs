//! Private temp files and atomic file replacement.

use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQ: AtomicU64 = AtomicU64::new(0);
static TEMP_ROOT: OnceLock<Result<PathBuf, String>> = OnceLock::new();

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{seq}", process::id())
}

/// Per-process directory (mode 0700) holding every temp file, so other local
/// users cannot pre-create or symlink the names we write to.
fn temp_root() -> Result<&'static Path, String> {
    TEMP_ROOT
        .get_or_init(|| {
            for _ in 0..16 {
                let dir = env::temp_dir().join(format!("s4-{}", unique_suffix()));
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                match builder.create(&dir) {
                    Ok(()) => return Ok(dir),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => {
                        return Err(format!("cannot create temp dir {}: {e}", dir.display()));
                    }
                }
            }
            Err("cannot create a unique temp dir".to_string())
        })
        .as_deref()
        .map_err(Clone::clone)
}

/// Removes the per-process temp directory; called once before exiting.
pub fn cleanup_temp_root() {
    if let Some(Ok(dir)) = TEMP_ROOT.get() {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A unique path in the private temp dir; the file or directory is removed on drop.
pub struct TempPath(PathBuf);

impl TempPath {
    pub fn new(tag: &str) -> Result<Self, String> {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(Self(temp_root()?.join(format!("{tag}-{seq}"))))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = if self.0.is_dir() {
            fs::remove_dir_all(&self.0)
        } else {
            fs::remove_file(&self.0)
        };
    }
}

/// A hidden sibling of `target` that replaces it only on [`commit`](Self::commit).
/// Dropped without committing, it is deleted and `target` is left untouched.
pub struct PartialFile {
    path: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl PartialFile {
    pub fn create(target: &Path) -> Result<Self, String> {
        let name = target
            .file_name()
            .ok_or_else(|| format!("invalid destination: {}", target.display()))?;
        let dir = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = dir.join(format!(
            ".{}.s4-partial-{}",
            name.to_string_lossy(),
            unique_suffix()
        ));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        Ok(Self {
            path,
            target: target.to_path_buf(),
            committed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn commit(mut self) -> Result<(), String> {
        fs::rename(&self.path, &self.target)
            .map_err(|e| format!("cannot move download into {}: {e}", self.target.display()))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Creates the parent directory of `path`, if it names one.
pub fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A fresh directory for a unit test, removed on drop. Tests do not use
/// [`TempPath`] because the per-process temp root is only cleaned by `main`.
#[cfg(test)]
pub struct TestDir(PathBuf);

#[cfg(test)]
impl TestDir {
    pub fn new() -> Self {
        let dir = env::temp_dir().join(format!("s4-test-{}", unique_suffix()));
        fs::create_dir(&dir).expect("create test dir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{PartialFile, TestDir};
    use std::fs;

    #[test]
    fn partial_file_leaves_target_untouched_unless_committed() {
        let dir = TestDir::new();
        let target = dir.path().join("out.txt");
        fs::write(&target, "original").expect("write");

        let partial = PartialFile::create(&target).expect("create partial");
        fs::write(partial.path(), "error body").expect("write partial");
        drop(partial);
        assert_eq!(fs::read_to_string(&target).expect("read"), "original");
        assert_eq!(fs::read_dir(dir.path()).expect("ls").count(), 1);

        let partial = PartialFile::create(&target).expect("create partial");
        fs::write(partial.path(), "new").expect("write partial");
        partial.commit().expect("commit");
        assert_eq!(fs::read_to_string(&target).expect("read"), "new");
    }
}
