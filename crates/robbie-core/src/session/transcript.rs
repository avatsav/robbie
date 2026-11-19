//! Explicit workspace exports, separate from the append-only diagnostics journal.
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn write(workspace: &Path, contents: &[u8]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        unix::write(workspace, contents)
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, contents);
        Err(io::Error::other("private transcript exports require Unix"))
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        fs::{self, File, OpenOptions},
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::OpenOptionsExt,
        },
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_DUMP: AtomicU64 = AtomicU64::new(0);

    pub(super) fn write(workspace: &Path, contents: &[u8]) -> io::Result<PathBuf> {
        let workspace = fs::canonicalize(workspace)?;
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&workspace)?;
        let application = directory(&root, c".robbie")?;
        let transcripts = directory(&application, c"transcripts")?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("system clock predates Unix epoch"))?
            .as_nanos();
        let name = format!(
            "transcript-{nanos:x}-{:x}-{:x}.json",
            std::process::id(),
            NEXT_DUMP.fetch_add(1, Ordering::Relaxed)
        );
        let name_c = CString::new(name.as_str()).map_err(io::Error::other)?;
        // SAFETY: the borrowed directory descriptor and NUL-terminated name are
        // live for this call. O_EXCL forbids overwriting/following an existing file.
        let fd = unsafe {
            libc::openat(
                transcripts.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor; File alone closes it.
        let mut file = unsafe { File::from_raw_fd(fd) };
        if let Err(error) = file.write_all(contents).and_then(|_| file.flush()) {
            // Best-effort removal of our incomplete export, relative to the same
            // anchored directory. Never fall back to a different destination.
            // SAFETY: the directory and C string are still live; flags=0 unlinks a file.
            unsafe {
                libc::unlinkat(transcripts.as_raw_fd(), name_c.as_ptr(), 0);
            }
            return Err(error);
        }
        Ok(workspace.join(".robbie/transcripts").join(name))
    }

    fn directory(parent: &File, name: &CStr) -> io::Result<File> {
        // SAFETY: both arguments are live and names are fixed single components.
        // mkdirat never changes the mode of an already existing directory.
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        // Resolve every component relative to its opened parent, not a joined
        // pathname: replacing .robbie with a symlink cannot redirect this open.
        // SAFETY: the parent descriptor and C string remain live for this call.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful openat descriptor is fresh and owned by this File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::{PermissionsExt, symlink};

        #[test]
        fn child_creation_uses_the_open_parent_even_if_its_path_is_replaced() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            fs::set_permissions(outside.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let anchor = File::open(root.path()).unwrap();
            let application = directory(&anchor, c".robbie").unwrap();
            fs::rename(root.path().join(".robbie"), root.path().join("held")).unwrap();
            symlink(outside.path(), root.path().join(".robbie")).unwrap();
            let _child = directory(&application, c"transcripts").unwrap();
            assert!(root.path().join("held/transcripts").is_dir());
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
            assert_eq!(
                fs::metadata(outside.path()).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}
