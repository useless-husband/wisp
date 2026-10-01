//! Capability-style path resolution.
//!
//! Every guest path is resolved one component at a time with `openat`/`readlinkat` relative
//! to a directory file descriptor the guest was given. The kernel is never asked to follow a
//! symlink or a `..` on our behalf: `..` pops a stack of directories we opened ourselves
//! (failing at the preopened root), and symlinks are read and their targets spliced into the
//! remaining path, so a link pointing outside (absolute, or with enough `..`) is caught by the
//! same rules. The final component is returned unresolved, together with the directory that
//! contains it, so callers use `*at` system calls with `O_NOFOLLOW`/`AT_SYMLINK_NOFOLLOW`.
//! Intermediate directories are opened with `O_NOFOLLOW`, so swapping one for a symlink after
//! the check makes the open fail instead of escaping.

use super::abi::*;
use std::collections::VecDeque;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// Symlink expansions allowed while resolving one path (same as Linux's limit).
const MAX_SYMLINKS: u32 = 40;

/// The directory containing the final component: either the caller's or one we opened.
pub enum Dir<'a> {
    Borrowed(RawFd, std::marker::PhantomData<&'a ()>),
    Owned(OwnedFd),
}

impl Dir<'_> {
    pub fn fd(&self) -> RawFd {
        match self {
            Dir::Borrowed(fd, _) => *fd,
            Dir::Owned(o) => o.as_raw_fd(),
        }
    }
}

pub fn cstr(s: &str) -> Result<CString, Errno> {
    CString::new(s).map_err(|_| ERRNO_ILSEQ)
}

pub fn last_errno() -> Errno {
    from_host_errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

/// `readlinkat(dir, name)`: `Ok(Some(target))` for a symlink, `Ok(None)` if not a symlink.
fn read_link(dir: RawFd, name: &str) -> Result<Option<String>, Errno> {
    let c = cstr(name)?;
    let mut buf = vec![0u8; 4096];
    let n = unsafe {
        libc::readlinkat(
            dir,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if n < 0 {
        let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if e == libc::EINVAL {
            return Ok(None);
        }
        return Err(from_host_errno(e));
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map(Some).map_err(|_| ERRNO_ILSEQ)
}

fn open_dir_nofollow(dir: RawFd, name: &str) -> Result<OwnedFd, Errno> {
    let c = cstr(name)?;
    let fd = unsafe {
        libc::openat(
            dir,
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // O_NOFOLLOW on a symlink reports ELOOP; a symlink appearing here means it was
        // swapped in after we checked, which we refuse rather than follow.
        return Err(if e == libc::ELOOP {
            ERRNO_NOTCAPABLE
        } else {
            from_host_errno(e)
        });
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Resolve `path` relative to `root`. Returns the directory holding the final component and
/// that component's name (`.` when the path names a directory itself, e.g. `a/` or `a/..`).
///
/// With `follow`, a final symlink is expanded too; otherwise it is returned as is.
pub fn resolve<'a>(root: RawFd, path: &str, follow: bool) -> Result<(Dir<'a>, String), Errno> {
    if path.is_empty() {
        return Err(ERRNO_NOENT);
    }
    if path.starts_with('/') {
        return Err(ERRNO_NOTCAPABLE);
    }
    if path.contains('\0') {
        return Err(ERRNO_ILSEQ);
    }
    let mut queue: VecDeque<String> = split(path);
    // Directories opened below `root`; the current directory is the last one (or `root`).
    let mut stack: Vec<OwnedFd> = Vec::new();
    let mut links = 0u32;
    let cur = |stack: &Vec<OwnedFd>| stack.last().map(|f| f.as_raw_fd()).unwrap_or(root);
    loop {
        let Some(c) = queue.pop_front() else {
            // Path ended in "." or ".." handling below always leaves something; but a path
            // made only of separators/dots resolves to the current directory itself.
            return Ok((into_dir(stack, root), ".".into()));
        };
        let last = queue.is_empty();
        match c.as_str() {
            "." => {
                if last {
                    return Ok((into_dir(stack, root), ".".into()));
                }
            }
            ".." => {
                if stack.pop().is_none() {
                    return Err(ERRNO_NOTCAPABLE);
                }
                if last {
                    return Ok((into_dir(stack, root), ".".into()));
                }
            }
            name => {
                if last && !follow {
                    return Ok((into_dir(stack, root), name.to_string()));
                }
                match read_link(cur(&stack), name) {
                    Ok(Some(target)) => {
                        links += 1;
                        if links > MAX_SYMLINKS {
                            return Err(ERRNO_LOOP);
                        }
                        if target.starts_with('/') {
                            return Err(ERRNO_NOTCAPABLE);
                        }
                        let mut t = split(&target);
                        // A trailing component of the link continues where the link was.
                        while let Some(x) = t.pop_back() {
                            queue.push_front(x);
                        }
                        if queue.is_empty() {
                            return Ok((into_dir(stack, root), ".".into()));
                        }
                    }
                    Ok(None) => {
                        if last {
                            return Ok((into_dir(stack, root), name.to_string()));
                        }
                        let fd = open_dir_nofollow(cur(&stack), name)?;
                        stack.push(fd);
                    }
                    Err(e) if e == ERRNO_NOENT && last => {
                        // Not there yet: fine for creation.
                        return Ok((into_dir(stack, root), name.to_string()));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

fn into_dir<'a>(mut stack: Vec<OwnedFd>, root: RawFd) -> Dir<'a> {
    match stack.pop() {
        Some(fd) => Dir::Owned(fd),
        None => Dir::Borrowed(root, std::marker::PhantomData),
    }
}

/// Split a path into components; a trailing `/` adds a final "." (the path must be a dir).
fn split(path: &str) -> VecDeque<String> {
    let mut q: VecDeque<String> = path
        .split('/')
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect();
    if path.ends_with('/') && !q.is_empty() {
        q.push_back(".".into());
    }
    if q.is_empty() {
        q.push_back(".".into());
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> (std::path::PathBuf, OwnedFd) {
        let base = std::env::temp_dir().join(format!(
            "wisp-sandbox-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("outside/secret.txt"), "secret").unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        symlink("../outside", root.join("escape")).unwrap();
        symlink("/etc", root.join("abs")).unwrap();
        symlink("loop", root.join("loop")).unwrap();
        symlink("sub/deeper", root.join("inside")).unwrap();
        symlink("../a.txt", root.join("sub/up")).unwrap();
        let c = CString::new(root.to_str().unwrap()).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        assert!(fd >= 0);
        (base, unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn name_of(r: Result<(Dir, String), Errno>) -> Result<String, Errno> {
        r.map(|(_, n)| n)
    }

    #[test]
    fn dotdot_cannot_leave_root() {
        let (base, root) = setup();
        let fd = root.as_raw_fd();
        assert_eq!(name_of(resolve(fd, "..", true)), Err(ERRNO_NOTCAPABLE));
        assert_eq!(
            name_of(resolve(fd, "../outside/secret.txt", true)),
            Err(ERRNO_NOTCAPABLE)
        );
        assert_eq!(
            name_of(resolve(fd, "sub/../../outside/secret.txt", true)),
            Err(ERRNO_NOTCAPABLE)
        );
        assert_eq!(
            name_of(resolve(fd, "sub/deeper/../../a.txt", true)),
            Ok("a.txt".into())
        );
        assert_eq!(
            name_of(resolve(fd, "/etc/passwd", true)),
            Err(ERRNO_NOTCAPABLE)
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn symlinks_cannot_escape() {
        let (base, root) = setup();
        let fd = root.as_raw_fd();
        assert_eq!(
            name_of(resolve(fd, "escape/secret.txt", true)),
            Err(ERRNO_NOTCAPABLE)
        );
        assert_eq!(name_of(resolve(fd, "escape", true)), Err(ERRNO_NOTCAPABLE));
        assert_eq!(
            name_of(resolve(fd, "abs/passwd", true)),
            Err(ERRNO_NOTCAPABLE)
        );
        assert_eq!(name_of(resolve(fd, "loop", true)), Err(ERRNO_LOOP));
        // Not following the final link returns the link itself (to be opened with O_NOFOLLOW).
        assert_eq!(name_of(resolve(fd, "escape", false)), Ok("escape".into()));
        // Links that stay inside work.
        assert_eq!(
            name_of(resolve(fd, "inside/../deeper", true)),
            Ok("deeper".into())
        );
        assert_eq!(name_of(resolve(fd, "sub/up", true)), Ok("a.txt".into()));
        std::fs::remove_dir_all(base).unwrap();
    }
}
