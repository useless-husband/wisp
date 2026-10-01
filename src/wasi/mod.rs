//! WASI preview 1 (`wasi_snapshot_preview1`).
//!
//! The guest sees only what it is given: its arguments, the environment variables passed in,
//! stdio, and the directories preopened for it. File system paths are resolved by
//! [`sandbox::resolve`], which keeps every lookup inside the preopened directory it starts from.
//! Sockets are not supported.

pub mod abi;
pub mod sandbox;

use crate::error::{Trap, TrapCode};
use crate::runtime::api::{Caller, Linker, Store};
use crate::runtime::values::Val;
use crate::types::{FuncType, ValType};
use abi::*;
use sandbox::{Dir, cstr, last_errno, resolve};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Where the guest's stdout or stderr goes.
#[derive(Clone)]
pub enum Output {
    Inherit,
    Buffer(Rc<RefCell<Vec<u8>>>),
    Discard,
}

/// Where the guest's stdin comes from.
pub enum Input {
    Inherit,
    Bytes(Vec<u8>, usize),
}

enum Fd {
    Stdin,
    Stdout,
    Stderr,
    Host {
        fd: OwnedFd,
        dir: bool,
        preopen: Option<String>,
        flags: u16,
        rights: (u64, u64),
    },
}

/// Per-guest WASI state; keep it in the store's data and implement [`WasiView`].
pub struct WasiCtx {
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    fds: Vec<Option<Fd>>,
    stdin: Input,
    stdout: Output,
    stderr: Output,
}

/// Gives the WASI host functions access to the context inside the store data.
pub trait WasiView {
    fn wasi(&mut self) -> &mut WasiCtx;
}

impl WasiView for WasiCtx {
    fn wasi(&mut self) -> &mut WasiCtx {
        self
    }
}

/// Builds a [`WasiCtx`].
pub struct WasiCtxBuilder {
    args: Vec<String>,
    env: Vec<(String, String)>,
    preopens: Vec<(OwnedFd, String)>,
    stdin: Input,
    stdout: Output,
    stderr: Output,
}

impl Default for WasiCtxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WasiCtxBuilder {
    pub fn new() -> Self {
        WasiCtxBuilder {
            args: Vec::new(),
            env: Vec::new(),
            preopens: Vec::new(),
            stdin: Input::Inherit,
            stdout: Output::Inherit,
            stderr: Output::Inherit,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<S: Into<String>>(mut self, a: impl IntoIterator<Item = S>) -> Self {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    /// Give the guest the host directory `host`, visible as `guest`.
    pub fn preopen_dir(
        mut self,
        host: impl AsRef<Path>,
        guest: impl Into<String>,
    ) -> std::io::Result<Self> {
        let p = host.as_ref();
        let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.preopens
            .push((unsafe { OwnedFd::from_raw_fd(fd) }, guest.into()));
        Ok(self)
    }

    pub fn stdin_bytes(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.stdin = Input::Bytes(b.into(), 0);
        self
    }

    pub fn stdout(mut self, o: Output) -> Self {
        self.stdout = o;
        self
    }

    pub fn stderr(mut self, o: Output) -> Self {
        self.stderr = o;
        self
    }

    pub fn build(self) -> WasiCtx {
        let mut fds = vec![Some(Fd::Stdin), Some(Fd::Stdout), Some(Fd::Stderr)];
        for (fd, name) in self.preopens {
            fds.push(Some(Fd::Host {
                fd,
                dir: true,
                preopen: Some(name),
                flags: 0,
                rights: (RIGHTS_ALL, RIGHTS_ALL),
            }));
        }
        WasiCtx {
            args: self.args.into_iter().map(|a| a.into_bytes()).collect(),
            env: self
                .env
                .into_iter()
                .map(|(k, v)| format!("{k}={v}").into_bytes())
                .collect(),
            fds,
            stdin: self.stdin,
            stdout: self.stdout,
            stderr: self.stderr,
        }
    }
}

/// Bounds-checked access to guest memory.
struct Mem<'a>(&'a mut [u8]);

type R<T = ()> = Result<T, Errno>;

impl Mem<'_> {
    fn range(&self, ptr: u32, len: u32) -> R<std::ops::Range<usize>> {
        let s = ptr as usize;
        let e = s.checked_add(len as usize).ok_or(ERRNO_FAULT)?;
        if e > self.0.len() {
            return Err(ERRNO_FAULT);
        }
        Ok(s..e)
    }
    fn slice(&self, ptr: u32, len: u32) -> R<&[u8]> {
        let r = self.range(ptr, len)?;
        Ok(&self.0[r])
    }
    fn slice_mut(&mut self, ptr: u32, len: u32) -> R<&mut [u8]> {
        let r = self.range(ptr, len)?;
        Ok(&mut self.0[r])
    }
    fn u32(&self, ptr: u32) -> R<u32> {
        Ok(u32::from_le_bytes(self.slice(ptr, 4)?.try_into().unwrap()))
    }
    fn u64(&self, ptr: u32) -> R<u64> {
        Ok(u64::from_le_bytes(self.slice(ptr, 8)?.try_into().unwrap()))
    }
    fn u16(&self, ptr: u32) -> R<u16> {
        Ok(u16::from_le_bytes(self.slice(ptr, 2)?.try_into().unwrap()))
    }
    fn put_u8(&mut self, ptr: u32, v: u8) -> R {
        self.slice_mut(ptr, 1)?[0] = v;
        Ok(())
    }
    fn put_u16(&mut self, ptr: u32, v: u16) -> R {
        self.slice_mut(ptr, 2)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn put_u32(&mut self, ptr: u32, v: u32) -> R {
        self.slice_mut(ptr, 4)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn put_u64(&mut self, ptr: u32, v: u64) -> R {
        self.slice_mut(ptr, 8)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn string(&self, ptr: u32, len: u32) -> R<String> {
        std::str::from_utf8(self.slice(ptr, len)?)
            .map(|s| s.to_string())
            .map_err(|_| ERRNO_ILSEQ)
    }
    /// The `(ptr, len)` pairs of an iovec array.
    fn iovs(&self, ptr: u32, n: u32) -> R<Vec<(u32, u32)>> {
        (0..n)
            .map(|i| Ok((self.u32(ptr + 8 * i)?, self.u32(ptr + 8 * i + 4)?)))
            .collect()
    }
}

fn a32(args: &[Val], i: usize) -> u32 {
    match args[i] {
        Val::I32(v) => v as u32,
        _ => 0,
    }
}

fn a64(args: &[Val], i: usize) -> u64 {
    match args[i] {
        Val::I64(v) => v as u64,
        _ => 0,
    }
}

fn check(r: libc::c_int) -> R<libc::c_int> {
    if r < 0 { Err(last_errno()) } else { Ok(r) }
}

fn check_size(r: isize) -> R<usize> {
    if r < 0 {
        Err(last_errno())
    } else {
        Ok(r as usize)
    }
}

fn fstat(fd: RawFd) -> R<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstat(fd, &mut st) })?;
    Ok(st)
}

fn write_filestat(m: &mut Mem, ptr: u32, st: &libc::stat) -> R {
    let ns = |s: i64, n: i64| {
        (s as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(n as u64)
    };
    #[cfg(target_os = "macos")]
    let (at, mt, ct) = (
        ns(st.st_atime, st.st_atime_nsec),
        ns(st.st_mtime, st.st_mtime_nsec),
        ns(st.st_ctime, st.st_ctime_nsec),
    );
    #[cfg(not(target_os = "macos"))]
    let (at, mt, ct) = (
        ns(st.st_atime as i64, st.st_atime_nsec as i64),
        ns(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ns(st.st_ctime as i64, st.st_ctime_nsec as i64),
    );
    m.slice_mut(ptr, 64)?.fill(0);
    m.put_u64(ptr, st.st_dev as u64)?;
    m.put_u64(ptr + 8, st.st_ino as u64)?;
    m.put_u8(ptr + 16, filetype_of_mode(st.st_mode as u32))?;
    m.put_u64(ptr + 24, st.st_nlink as u64)?;
    m.put_u64(ptr + 32, st.st_size as u64)?;
    m.put_u64(ptr + 40, at)?;
    m.put_u64(ptr + 48, mt)?;
    m.put_u64(ptr + 56, ct)
}

fn timespecs(atim: u64, mtim: u64, fst: u16) -> R<[libc::timespec; 2]> {
    if (fst & FSTFLAGS_ATIM != 0 && fst & FSTFLAGS_ATIM_NOW != 0)
        || (fst & FSTFLAGS_MTIM != 0 && fst & FSTFLAGS_MTIM_NOW != 0)
    {
        return Err(ERRNO_INVAL);
    }
    let one = |t: u64, set: u16, now: u16| {
        if fst & now != 0 {
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW,
            }
        } else if fst & set != 0 {
            libc::timespec {
                tv_sec: (t / 1_000_000_000) as _,
                tv_nsec: (t % 1_000_000_000) as _,
            }
        } else {
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            }
        }
    };
    Ok([
        one(atim, FSTFLAGS_ATIM, FSTFLAGS_ATIM_NOW),
        one(mtim, FSTFLAGS_MTIM, FSTFLAGS_MTIM_NOW),
    ])
}

fn clock_id(id: u32) -> R<libc::clockid_t> {
    Ok(match id {
        CLOCK_REALTIME => libc::CLOCK_REALTIME,
        CLOCK_MONOTONIC => libc::CLOCK_MONOTONIC,
        CLOCK_PROCESS_CPUTIME => libc::CLOCK_PROCESS_CPUTIME_ID,
        CLOCK_THREAD_CPUTIME => libc::CLOCK_THREAD_CPUTIME_ID,
        _ => return Err(ERRNO_INVAL),
    })
}

fn now_ns(id: u32) -> R<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    check(unsafe { libc::clock_gettime(clock_id(id)?, &mut ts) })?;
    Ok(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

impl WasiCtx {
    fn host_fd(&self, fd: u32) -> R<RawFd> {
        match self.fds.get(fd as usize) {
            Some(Some(Fd::Host { fd, .. })) => Ok(fd.as_raw_fd()),
            Some(Some(_)) => Err(ERRNO_SPIPE),
            _ => Err(ERRNO_BADF),
        }
    }

    fn dir_fd(&self, fd: u32) -> R<RawFd> {
        match self.fds.get(fd as usize) {
            Some(Some(Fd::Host { fd, dir: true, .. })) => Ok(fd.as_raw_fd()),
            Some(Some(_)) => Err(ERRNO_NOTDIR),
            _ => Err(ERRNO_BADF),
        }
    }

    fn exists(&self, fd: u32) -> R {
        match self.fds.get(fd as usize) {
            Some(Some(_)) => Ok(()),
            _ => Err(ERRNO_BADF),
        }
    }

    fn alloc_fd(&mut self, f: Fd) -> u32 {
        if let Some(i) = self.fds.iter().skip(3).position(|e| e.is_none()) {
            self.fds[i + 3] = Some(f);
            return (i + 3) as u32;
        }
        self.fds.push(Some(f));
        (self.fds.len() - 1) as u32
    }

    fn write_out(o: &Output, data: &[u8], err: bool) -> R<usize> {
        match o {
            Output::Inherit => {
                let r = if err {
                    std::io::stderr().write_all(data)
                } else {
                    std::io::stdout().write_all(data)
                };
                r.map_err(|_| ERRNO_IO)?;
            }
            Output::Buffer(b) => b.borrow_mut().extend_from_slice(data),
            Output::Discard => {}
        }
        Ok(data.len())
    }

    /// Flush inherited stdout (call before the process exits).
    pub fn flush(&self) {
        let _ = std::io::stdout().flush();
    }
}

fn list_strings(m: &mut Mem, items: &[Vec<u8>], ptrs: u32, buf: u32) -> R {
    let mut p = buf;
    for (i, s) in items.iter().enumerate() {
        m.put_u32(ptrs + 4 * i as u32, p)?;
        m.slice_mut(p, s.len() as u32)?.copy_from_slice(s);
        m.put_u8(p + s.len() as u32, 0)?;
        p += s.len() as u32 + 1;
    }
    Ok(())
}

fn sizes(m: &mut Mem, items: &[Vec<u8>], count: u32, size: u32) -> R {
    m.put_u32(count, items.len() as u32)?;
    m.put_u32(size, items.iter().map(|s| s.len() as u32 + 1).sum())
}

type Impl = fn(&mut WasiCtx, &mut Mem, &[Val]) -> R;

fn args_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    list_strings(m, &w.args, a32(a, 0), a32(a, 1))
}
fn args_sizes_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    sizes(m, &w.args, a32(a, 0), a32(a, 1))
}
fn environ_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    list_strings(m, &w.env, a32(a, 0), a32(a, 1))
}
fn environ_sizes_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    sizes(m, &w.env, a32(a, 0), a32(a, 1))
}

fn clock_res_get(_: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    check(unsafe { libc::clock_getres(clock_id(a32(a, 0))?, &mut ts) })?;
    m.put_u64(
        a32(a, 1),
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64,
    )
}

fn clock_time_get(_: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let t = now_ns(a32(a, 0))?;
    m.put_u64(a32(a, 2), t)
}

fn fd_advise(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    w.exists(a32(a, 0))
}

fn fd_allocate(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let end = a64(a, 1).checked_add(a64(a, 2)).ok_or(ERRNO_FBIG)?;
    let st = fstat(fd)?;
    if (st.st_size as u64) < end {
        check(unsafe { libc::ftruncate(fd, end as libc::off_t) })?;
    }
    Ok(())
}

fn fd_close(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let fd = a32(a, 0) as usize;
    match w.fds.get_mut(fd) {
        Some(e @ Some(_)) => {
            *e = None;
            Ok(())
        }
        _ => Err(ERRNO_BADF),
    }
}

fn fd_sync(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    check(unsafe { libc::fsync(fd) })?;
    Ok(())
}

fn fd_fdstat_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let ptr = a32(a, 1);
    let (ft, flags, rights) = match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Host {
            fd, flags, rights, ..
        })) => (
            filetype_of_mode(fstat(fd.as_raw_fd())?.st_mode as u32),
            *flags,
            *rights,
        ),
        Some(Some(Fd::Stdout | Fd::Stderr)) => {
            (FILETYPE_CHARACTER_DEVICE, FDFLAGS_APPEND, (RIGHTS_ALL, 0))
        }
        Some(Some(Fd::Stdin)) => (FILETYPE_CHARACTER_DEVICE, 0, (RIGHTS_ALL, 0)),
        _ => return Err(ERRNO_BADF),
    };
    m.slice_mut(ptr, 24)?.fill(0);
    m.put_u8(ptr, ft)?;
    m.put_u16(ptr + 2, flags)?;
    m.put_u64(ptr + 8, rights.0)?;
    m.put_u64(ptr + 16, rights.1)
}

fn fd_fdstat_set_flags(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let new = a32(a, 1) as u16;
    match w.fds.get_mut(a32(a, 0) as usize) {
        Some(Some(Fd::Host { fd, flags, .. })) => {
            let mut fl = check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) })?;
            fl &= !(libc::O_APPEND | libc::O_NONBLOCK);
            if new & FDFLAGS_APPEND != 0 {
                fl |= libc::O_APPEND;
            }
            if new & FDFLAGS_NONBLOCK != 0 {
                fl |= libc::O_NONBLOCK;
            }
            check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl) })?;
            *flags = new;
            Ok(())
        }
        Some(Some(_)) => Ok(()),
        _ => Err(ERRNO_BADF),
    }
}

fn fd_fdstat_set_rights(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let (base, inh) = (a64(a, 1), a64(a, 2));
    match w.fds.get_mut(a32(a, 0) as usize) {
        Some(Some(Fd::Host { rights, .. })) => {
            // Rights can only be dropped.
            if base & !rights.0 != 0 || inh & !rights.1 != 0 {
                return Err(ERRNO_NOTCAPABLE);
            }
            *rights = (base, inh);
            Ok(())
        }
        Some(Some(_)) => Ok(()),
        _ => Err(ERRNO_BADF),
    }
}

fn fd_filestat_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let ptr = a32(a, 1);
    match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Host { fd, .. })) => {
            let st = fstat(fd.as_raw_fd())?;
            write_filestat(m, ptr, &st)
        }
        Some(Some(_)) => {
            m.slice_mut(ptr, 64)?.fill(0);
            m.put_u8(ptr + 16, FILETYPE_CHARACTER_DEVICE)
        }
        _ => Err(ERRNO_BADF),
    }
}

fn fd_filestat_set_size(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    check(unsafe { libc::ftruncate(fd, a64(a, 1) as libc::off_t) })?;
    Ok(())
}

fn fd_filestat_set_times(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let ts = timespecs(a64(a, 1), a64(a, 2), a32(a, 3) as u16)?;
    check(unsafe { libc::futimens(fd, ts.as_ptr()) })?;
    Ok(())
}

fn fd_pread(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let iovs = m.iovs(a32(a, 1), a32(a, 2))?;
    let mut off = a64(a, 3);
    let mut total = 0usize;
    for (p, l) in iovs {
        let buf = m.slice_mut(p, l)?;
        let n = check_size(unsafe {
            libc::pread(
                fd,
                buf.as_mut_ptr() as *mut _,
                buf.len(),
                off as libc::off_t,
            )
        })?;
        total += n;
        off += n as u64;
        if n < l as usize {
            break;
        }
    }
    m.put_u32(a32(a, 4), total as u32)
}

fn fd_pwrite(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let iovs = m.iovs(a32(a, 1), a32(a, 2))?;
    let mut off = a64(a, 3);
    let mut total = 0usize;
    for (p, l) in iovs {
        let buf = m.slice(p, l)?;
        let n = check_size(unsafe {
            libc::pwrite(fd, buf.as_ptr() as *const _, buf.len(), off as libc::off_t)
        })?;
        total += n;
        off += n as u64;
        if n < l as usize {
            break;
        }
    }
    m.put_u32(a32(a, 4), total as u32)
}

fn fd_prestat_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Host {
            preopen: Some(name),
            ..
        })) => {
            let ptr = a32(a, 1);
            m.put_u32(ptr, 0)?;
            m.put_u32(ptr + 4, name.len() as u32)
        }
        _ => Err(ERRNO_BADF),
    }
}

fn fd_prestat_dir_name(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Host {
            preopen: Some(name),
            ..
        })) => {
            let len = a32(a, 2) as usize;
            if len < name.len() {
                return Err(ERRNO_NAMETOOLONG);
            }
            m.slice_mut(a32(a, 1), name.len() as u32)?
                .copy_from_slice(name.as_bytes());
            Ok(())
        }
        _ => Err(ERRNO_BADF),
    }
}

fn fd_read(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let iovs = m.iovs(a32(a, 1), a32(a, 2))?;
    let mut total = 0usize;
    match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Stdin)) => {
            for (p, l) in iovs {
                let buf = m.slice_mut(p, l)?;
                let n = match &mut w.stdin {
                    Input::Inherit => std::io::stdin().read(buf).map_err(|_| ERRNO_IO)?,
                    Input::Bytes(b, pos) => {
                        let n = buf.len().min(b.len() - *pos);
                        buf[..n].copy_from_slice(&b[*pos..*pos + n]);
                        *pos += n;
                        n
                    }
                };
                total += n;
                if n < l as usize {
                    break;
                }
            }
        }
        Some(Some(Fd::Host { fd, dir: false, .. })) => {
            let fd = fd.as_raw_fd();
            for (p, l) in iovs {
                let buf = m.slice_mut(p, l)?;
                let n =
                    check_size(unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) })?;
                total += n;
                if n < l as usize {
                    break;
                }
            }
        }
        Some(Some(Fd::Host { dir: true, .. })) => return Err(ERRNO_ISDIR),
        Some(Some(_)) => return Err(ERRNO_BADF),
        _ => return Err(ERRNO_BADF),
    }
    m.put_u32(a32(a, 3), total as u32)
}

fn fd_write(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let iovs = m.iovs(a32(a, 1), a32(a, 2))?;
    let mut data = Vec::new();
    for (p, l) in &iovs {
        data.extend_from_slice(m.slice(*p, *l)?);
    }
    let n = match w.fds.get(a32(a, 0) as usize) {
        Some(Some(Fd::Stdout)) => WasiCtx::write_out(&w.stdout, &data, false)?,
        Some(Some(Fd::Stderr)) => WasiCtx::write_out(&w.stderr, &data, true)?,
        Some(Some(Fd::Host { fd, dir: false, .. })) => check_size(unsafe {
            libc::write(fd.as_raw_fd(), data.as_ptr() as *const _, data.len())
        })?,
        Some(Some(Fd::Host { dir: true, .. })) => return Err(ERRNO_ISDIR),
        _ => return Err(ERRNO_BADF),
    };
    m.put_u32(a32(a, 3), n as u32)
}

fn fd_readdir(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let fd = w.dir_fd(a32(a, 0))?;
    let (buf, buf_len, cookie, used_ptr) = (a32(a, 1), a32(a, 2), a64(a, 3), a32(a, 4));
    // Read the whole directory through a fresh descriptor so the stream position of `fd`
    // does not matter; the cookie is the index of the next entry.
    let dfd = check(unsafe {
        libc::openat(
            fd,
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    let dir = unsafe { libc::fdopendir(dfd) };
    if dir.is_null() {
        unsafe { libc::close(dfd) };
        return Err(last_errno());
    }
    let mut entries: Vec<(Vec<u8>, u64, u8)> = Vec::new();
    loop {
        let e = unsafe { libc::readdir(dir) };
        if e.is_null() {
            break;
        }
        let e = unsafe { &*e };
        let name = unsafe { std::ffi::CStr::from_ptr(e.d_name.as_ptr()) }
            .to_bytes()
            .to_vec();
        entries.push((name, e.d_ino as u64, filetype_of_dtype(e.d_type)));
    }
    unsafe { libc::closedir(dir) };
    entries.sort_by(|x, y| x.0.cmp(&y.0));
    let mut used = 0u32;
    for (i, (name, ino, ty)) in entries.iter().enumerate().skip(cookie as usize) {
        let mut ent = Vec::with_capacity(24 + name.len());
        ent.extend_from_slice(&(i as u64 + 1).to_le_bytes());
        ent.extend_from_slice(&ino.to_le_bytes());
        ent.extend_from_slice(&(name.len() as u32).to_le_bytes());
        ent.extend_from_slice(&[*ty, 0, 0, 0]);
        ent.extend_from_slice(name);
        let room = (buf_len - used) as usize;
        let n = ent.len().min(room);
        m.slice_mut(buf + used, n as u32)?
            .copy_from_slice(&ent[..n]);
        used += n as u32;
        if n < ent.len() {
            break;
        }
    }
    m.put_u32(used_ptr, used)
}

fn fd_renumber(w: &mut WasiCtx, _: &mut Mem, a: &[Val]) -> R {
    let (from, to) = (a32(a, 0) as usize, a32(a, 1) as usize);
    w.exists(from as u32)?;
    w.exists(to as u32)?;
    if from != to {
        let f = w.fds[from].take();
        w.fds[to] = f;
    }
    Ok(())
}

fn fd_seek(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let whence = match a32(a, 2) as u8 {
        WHENCE_SET => libc::SEEK_SET,
        WHENCE_CUR => libc::SEEK_CUR,
        WHENCE_END => libc::SEEK_END,
        _ => return Err(ERRNO_INVAL),
    };
    let r = unsafe { libc::lseek(fd, a64(a, 1) as i64 as libc::off_t, whence) };
    if r < 0 {
        return Err(last_errno());
    }
    m.put_u64(a32(a, 3), r as u64)
}

fn fd_tell(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let fd = w.host_fd(a32(a, 0))?;
    let r = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
    if r < 0 {
        return Err(last_errno());
    }
    m.put_u64(a32(a, 1), r as u64)
}

/// Resolve the path argument at `(ptr, len)` relative to directory fd `fd`.
fn path_arg<'a>(
    w: &WasiCtx,
    m: &Mem,
    fd: u32,
    ptr: u32,
    len: u32,
    follow: bool,
) -> R<(Dir<'a>, std::ffi::CString)> {
    let root = w.dir_fd(fd)?;
    let path = m.string(ptr, len)?;
    let (dir, name) = resolve(root, &path, follow)?;
    let c = cstr(&name)?;
    Ok((dir, c))
}

fn path_create_directory(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 1), a32(a, 2), false)?;
    check(unsafe { libc::mkdirat(dir.fd(), name.as_ptr(), 0o777) })?;
    Ok(())
}

fn path_filestat_get(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let follow = a32(a, 1) & LOOKUP_SYMLINK_FOLLOW != 0;
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 2), a32(a, 3), follow)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstatat(dir.fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    write_filestat(m, a32(a, 4), &st)
}

fn path_filestat_set_times(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let follow = a32(a, 1) & LOOKUP_SYMLINK_FOLLOW != 0;
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 2), a32(a, 3), follow)?;
    let ts = timespecs(a64(a, 4), a64(a, 5), a32(a, 6) as u16)?;
    check(unsafe {
        libc::utimensat(
            dir.fd(),
            name.as_ptr(),
            ts.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(())
}

fn path_link(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let follow = a32(a, 1) & LOOKUP_SYMLINK_FOLLOW != 0;
    let (od, on) = path_arg(w, m, a32(a, 0), a32(a, 2), a32(a, 3), follow)?;
    let (nd, nn) = path_arg(w, m, a32(a, 4), a32(a, 5), a32(a, 6), false)?;
    check(unsafe { libc::linkat(od.fd(), on.as_ptr(), nd.fd(), nn.as_ptr(), 0) })?;
    Ok(())
}

fn path_open(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let follow = a32(a, 1) & LOOKUP_SYMLINK_FOLLOW != 0;
    let oflags = a32(a, 4) as u16;
    let (rights_base, rights_inh) = (a64(a, 5), a64(a, 6));
    let fdflags = a32(a, 7) as u16;
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 2), a32(a, 3), follow)?;
    // Symlinks were expanded by the resolver when asked to; never let the kernel follow one.
    let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let read = rights_base & RIGHT_FD_READ != 0;
    let write = rights_base & RIGHT_FD_WRITE != 0;
    if oflags & OFLAGS_DIRECTORY != 0 {
        flags |= libc::O_DIRECTORY | libc::O_RDONLY;
    } else if write && read {
        flags |= libc::O_RDWR;
    } else if write {
        flags |= libc::O_WRONLY;
    } else {
        flags |= libc::O_RDONLY;
    }
    if oflags & OFLAGS_CREAT != 0 {
        flags |= libc::O_CREAT;
    }
    if oflags & OFLAGS_EXCL != 0 {
        flags |= libc::O_EXCL;
    }
    if oflags & OFLAGS_TRUNC != 0 {
        flags |= libc::O_TRUNC;
    }
    if fdflags & FDFLAGS_APPEND != 0 {
        flags |= libc::O_APPEND;
    }
    if fdflags & FDFLAGS_NONBLOCK != 0 {
        flags |= libc::O_NONBLOCK;
    }
    if fdflags & (FDFLAGS_SYNC | FDFLAGS_RSYNC) != 0 {
        flags |= libc::O_SYNC;
    }
    if fdflags & FDFLAGS_DSYNC != 0 {
        flags |= libc::O_DSYNC;
    }
    let raw = unsafe { libc::openat(dir.fd(), name.as_ptr(), flags, 0o666 as libc::c_uint) };
    if raw < 0 {
        return Err(last_errno());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let is_dir = filetype_of_mode(fstat(raw)?.st_mode as u32) == FILETYPE_DIRECTORY;
    let n = w.alloc_fd(Fd::Host {
        fd,
        dir: is_dir,
        preopen: None,
        flags: fdflags,
        rights: (rights_base, rights_inh),
    });
    m.put_u32(a32(a, 8), n)
}

fn path_readlink(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 1), a32(a, 2), false)?;
    let (buf, len) = (a32(a, 3), a32(a, 4));
    let mut tmp = vec![0u8; 4096];
    let n = check_size(unsafe {
        libc::readlinkat(
            dir.fd(),
            name.as_ptr(),
            tmp.as_mut_ptr() as *mut libc::c_char,
            tmp.len(),
        )
    })?;
    let n = n.min(len as usize);
    m.slice_mut(buf, n as u32)?.copy_from_slice(&tmp[..n]);
    m.put_u32(a32(a, 5), n as u32)
}

fn path_remove_directory(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 1), a32(a, 2), false)?;
    check(unsafe { libc::unlinkat(dir.fd(), name.as_ptr(), libc::AT_REMOVEDIR) })?;
    Ok(())
}

fn path_rename(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (od, on) = path_arg(w, m, a32(a, 0), a32(a, 1), a32(a, 2), false)?;
    let (nd, nn) = path_arg(w, m, a32(a, 3), a32(a, 4), a32(a, 5), false)?;
    check(unsafe { libc::renameat(od.fd(), on.as_ptr(), nd.fd(), nn.as_ptr()) })?;
    Ok(())
}

fn path_symlink(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let target = m.string(a32(a, 0), a32(a, 1))?;
    let (dir, name) = path_arg(w, m, a32(a, 2), a32(a, 3), a32(a, 4), false)?;
    let t = cstr(&target)?;
    check(unsafe { libc::symlinkat(t.as_ptr(), dir.fd(), name.as_ptr()) })?;
    Ok(())
}

fn path_unlink_file(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (dir, name) = path_arg(w, m, a32(a, 0), a32(a, 1), a32(a, 2), false)?;
    check(unsafe { libc::unlinkat(dir.fd(), name.as_ptr(), 0) })?;
    Ok(())
}

fn poll_oneoff(w: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let (inp, out, n, nevents_ptr) = (a32(a, 0), a32(a, 1), a32(a, 2), a32(a, 3));
    if n == 0 {
        return Err(ERRNO_INVAL);
    }
    let start = Instant::now();
    let mut clocks: Vec<(u64, Duration)> = Vec::new();
    let mut ready: Vec<(u64, u8, u16)> = Vec::new();
    for i in 0..n {
        let s = inp + 48 * i;
        let userdata = m.u64(s)?;
        let tag = m.slice(s + 8, 1)?[0];
        match tag {
            EVENTTYPE_CLOCK => {
                let id = m.u32(s + 16)?;
                let timeout = m.u64(s + 24)?;
                let flags = m.u16(s + 40)?;
                let rel = if flags & SUBCLOCKFLAGS_ABSTIME != 0 {
                    timeout.saturating_sub(now_ns(id)?)
                } else {
                    clock_id(id)?;
                    timeout
                };
                clocks.push((userdata, Duration::from_nanos(rel)));
            }
            EVENTTYPE_FD_READ | EVENTTYPE_FD_WRITE => {
                let fd = m.u32(s + 16)?;
                let err = if w.exists(fd).is_ok() {
                    ERRNO_SUCCESS
                } else {
                    ERRNO_BADF
                };
                ready.push((userdata, tag, err));
            }
            _ => return Err(ERRNO_INVAL),
        }
    }
    if ready.is_empty()
        && let Some(min) = clocks.iter().map(|c| c.1).min()
    {
        std::thread::sleep(min);
    }
    let elapsed = start.elapsed();
    let mut events: Vec<(u64, u16, u8)> = ready.into_iter().map(|(u, t, e)| (u, e, t)).collect();
    for (u, d) in clocks {
        if d <= elapsed {
            events.push((u, ERRNO_SUCCESS, EVENTTYPE_CLOCK));
        }
    }
    for (i, (u, e, t)) in events.iter().enumerate() {
        let p = out + 32 * i as u32;
        m.slice_mut(p, 32)?.fill(0);
        m.put_u64(p, *u)?;
        m.put_u16(p + 8, *e)?;
        m.put_u8(p + 10, *t)?;
        if *t != EVENTTYPE_CLOCK {
            m.put_u64(p + 16, 1)?;
        }
    }
    m.put_u32(nevents_ptr, events.len() as u32)
}

fn sched_yield(_: &mut WasiCtx, _: &mut Mem, _: &[Val]) -> R {
    std::thread::yield_now();
    Ok(())
}

fn random_get(_: &mut WasiCtx, m: &mut Mem, a: &[Val]) -> R {
    let buf = m.slice_mut(a32(a, 0), a32(a, 1))?;
    for chunk in buf.chunks_mut(256) {
        check(unsafe { libc::getentropy(chunk.as_mut_ptr() as *mut _, chunk.len()) })?;
    }
    Ok(())
}

fn not_supported(_: &mut WasiCtx, _: &mut Mem, _: &[Val]) -> R {
    Err(ERRNO_NOTSUP)
}

/// (name, params ('i' = i32, 'I' = i64), implementation). All return an i32 errno.
const FUNCS: &[(&str, &str, Impl)] = &[
    ("args_get", "ii", args_get),
    ("args_sizes_get", "ii", args_sizes_get),
    ("environ_get", "ii", environ_get),
    ("environ_sizes_get", "ii", environ_sizes_get),
    ("clock_res_get", "ii", clock_res_get),
    ("clock_time_get", "iIi", clock_time_get),
    ("fd_advise", "iIIi", fd_advise),
    ("fd_allocate", "iII", fd_allocate),
    ("fd_close", "i", fd_close),
    ("fd_datasync", "i", fd_sync),
    ("fd_fdstat_get", "ii", fd_fdstat_get),
    ("fd_fdstat_set_flags", "ii", fd_fdstat_set_flags),
    ("fd_fdstat_set_rights", "iII", fd_fdstat_set_rights),
    ("fd_filestat_get", "ii", fd_filestat_get),
    ("fd_filestat_set_size", "iI", fd_filestat_set_size),
    ("fd_filestat_set_times", "iIIi", fd_filestat_set_times),
    ("fd_pread", "iiiIi", fd_pread),
    ("fd_prestat_get", "ii", fd_prestat_get),
    ("fd_prestat_dir_name", "iii", fd_prestat_dir_name),
    ("fd_pwrite", "iiiIi", fd_pwrite),
    ("fd_read", "iiii", fd_read),
    ("fd_readdir", "iiiIi", fd_readdir),
    ("fd_renumber", "ii", fd_renumber),
    ("fd_seek", "iIii", fd_seek),
    ("fd_sync", "i", fd_sync),
    ("fd_tell", "ii", fd_tell),
    ("fd_write", "iiii", fd_write),
    ("path_create_directory", "iii", path_create_directory),
    ("path_filestat_get", "iiiii", path_filestat_get),
    (
        "path_filestat_set_times",
        "iiiiIIi",
        path_filestat_set_times,
    ),
    ("path_link", "iiiiiii", path_link),
    ("path_open", "iiiiiIIii", path_open),
    ("path_readlink", "iiiiii", path_readlink),
    ("path_remove_directory", "iii", path_remove_directory),
    ("path_rename", "iiiiii", path_rename),
    ("path_symlink", "iiiii", path_symlink),
    ("path_unlink_file", "iii", path_unlink_file),
    ("poll_oneoff", "iiii", poll_oneoff),
    ("proc_raise", "i", not_supported),
    ("sched_yield", "", sched_yield),
    ("random_get", "ii", random_get),
    ("sock_accept", "iii", not_supported),
    ("sock_recv", "iiiiii", not_supported),
    ("sock_send", "iiiii", not_supported),
    ("sock_shutdown", "ii", not_supported),
];

/// Module name the functions are defined under.
pub const MODULE: &str = "wasi_snapshot_preview1";

/// Define every WASI preview 1 function in `linker`.
pub fn add_to_linker<T: WasiView + 'static>(linker: &mut Linker<T>, store: &mut Store<T>) {
    for &(name, sig, f) in FUNCS {
        let params: Vec<ValType> = sig
            .chars()
            .map(|c| if c == 'I' { ValType::I64 } else { ValType::I32 })
            .collect();
        let ty = FuncType::new(params, vec![ValType::I32]);
        linker.func(
            store,
            MODULE,
            name,
            ty,
            move |mut caller: Caller<'_, T>, args: &[Val], results: &mut [Val]| {
                let (mem, data) = caller.memory_and_data();
                let mut mem = Mem(mem);
                let errno = match f(data.wasi(), &mut mem, args) {
                    Ok(()) => ERRNO_SUCCESS,
                    Err(e) => e,
                };
                results[0] = Val::I32(errno as i32);
                Ok(())
            },
        );
    }
    let ty = FuncType::new(vec![ValType::I32], vec![]);
    linker.func(
        store,
        MODULE,
        "proc_exit",
        ty,
        |mut caller: Caller<'_, T>, args: &[Val], _: &mut [Val]| {
            caller.data_mut().wasi().flush();
            Err(Trap::new(TrapCode::Exit(a32(args, 0) as i32)))
        },
    );
}
