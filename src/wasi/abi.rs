//! WASI preview 1 constants and layouts (`wasi_snapshot_preview1`).

pub type Errno = u16;

pub const ERRNO_SUCCESS: Errno = 0;
pub const ERRNO_2BIG: Errno = 1;
pub const ERRNO_ACCES: Errno = 2;
pub const ERRNO_AGAIN: Errno = 6;
pub const ERRNO_BADF: Errno = 8;
pub const ERRNO_BUSY: Errno = 10;
pub const ERRNO_EXIST: Errno = 20;
pub const ERRNO_FAULT: Errno = 21;
pub const ERRNO_FBIG: Errno = 22;
pub const ERRNO_ILSEQ: Errno = 25;
pub const ERRNO_INTR: Errno = 27;
pub const ERRNO_INVAL: Errno = 28;
pub const ERRNO_IO: Errno = 29;
pub const ERRNO_ISDIR: Errno = 31;
pub const ERRNO_LOOP: Errno = 32;
pub const ERRNO_MFILE: Errno = 33;
pub const ERRNO_MLINK: Errno = 34;
pub const ERRNO_NAMETOOLONG: Errno = 37;
pub const ERRNO_NFILE: Errno = 41;
pub const ERRNO_NODEV: Errno = 43;
pub const ERRNO_NOENT: Errno = 44;
pub const ERRNO_NOMEM: Errno = 48;
pub const ERRNO_NOSPC: Errno = 51;
pub const ERRNO_NOSYS: Errno = 52;
pub const ERRNO_NOTDIR: Errno = 54;
pub const ERRNO_NOTEMPTY: Errno = 55;
pub const ERRNO_NOTSUP: Errno = 58;
pub const ERRNO_NOTTY: Errno = 59;
pub const ERRNO_NXIO: Errno = 60;
pub const ERRNO_OVERFLOW: Errno = 61;
pub const ERRNO_PERM: Errno = 63;
pub const ERRNO_PIPE: Errno = 64;
pub const ERRNO_RANGE: Errno = 68;
pub const ERRNO_ROFS: Errno = 69;
pub const ERRNO_SPIPE: Errno = 70;
pub const ERRNO_TXTBSY: Errno = 74;
pub const ERRNO_XDEV: Errno = 75;
pub const ERRNO_NOTCAPABLE: Errno = 76;

pub fn from_host_errno(e: i32) -> Errno {
    match e {
        0 => ERRNO_IO,
        libc::E2BIG => ERRNO_2BIG,
        libc::EACCES => ERRNO_ACCES,
        libc::EAGAIN => ERRNO_AGAIN,
        libc::EBADF => ERRNO_BADF,
        libc::EBUSY => ERRNO_BUSY,
        libc::EEXIST => ERRNO_EXIST,
        libc::EFAULT => ERRNO_FAULT,
        libc::EFBIG => ERRNO_FBIG,
        libc::EILSEQ => ERRNO_ILSEQ,
        libc::EINTR => ERRNO_INTR,
        libc::EINVAL => ERRNO_INVAL,
        libc::EIO => ERRNO_IO,
        libc::EISDIR => ERRNO_ISDIR,
        libc::ELOOP => ERRNO_LOOP,
        libc::EMFILE => ERRNO_MFILE,
        libc::EMLINK => ERRNO_MLINK,
        libc::ENAMETOOLONG => ERRNO_NAMETOOLONG,
        libc::ENFILE => ERRNO_NFILE,
        libc::ENODEV => ERRNO_NODEV,
        libc::ENOENT => ERRNO_NOENT,
        libc::ENOMEM => ERRNO_NOMEM,
        libc::ENOSPC => ERRNO_NOSPC,
        libc::ENOSYS => ERRNO_NOSYS,
        libc::ENOTDIR => ERRNO_NOTDIR,
        libc::ENOTEMPTY => ERRNO_NOTEMPTY,
        libc::ENOTSUP => ERRNO_NOTSUP,
        libc::ENOTTY => ERRNO_NOTTY,
        libc::ENXIO => ERRNO_NXIO,
        libc::EOVERFLOW => ERRNO_OVERFLOW,
        libc::EPERM => ERRNO_PERM,
        libc::EPIPE => ERRNO_PIPE,
        libc::ERANGE => ERRNO_RANGE,
        libc::EROFS => ERRNO_ROFS,
        libc::ESPIPE => ERRNO_SPIPE,
        libc::ETXTBSY => ERRNO_TXTBSY,
        libc::EXDEV => ERRNO_XDEV,
        #[allow(unreachable_patterns)]
        libc::EOPNOTSUPP => ERRNO_NOTSUP,
        _ => ERRNO_IO,
    }
}

// File types.
pub const FILETYPE_UNKNOWN: u8 = 0;
pub const FILETYPE_BLOCK_DEVICE: u8 = 1;
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;
pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;
pub const FILETYPE_SOCKET_STREAM: u8 = 6;
pub const FILETYPE_SYMBOLIC_LINK: u8 = 7;

pub fn filetype_of_mode(mode: u32) -> u8 {
    match mode & libc::S_IFMT as u32 {
        x if x == libc::S_IFDIR as u32 => FILETYPE_DIRECTORY,
        x if x == libc::S_IFREG as u32 => FILETYPE_REGULAR_FILE,
        x if x == libc::S_IFLNK as u32 => FILETYPE_SYMBOLIC_LINK,
        x if x == libc::S_IFCHR as u32 => FILETYPE_CHARACTER_DEVICE,
        x if x == libc::S_IFBLK as u32 => FILETYPE_BLOCK_DEVICE,
        x if x == libc::S_IFSOCK as u32 => FILETYPE_SOCKET_STREAM,
        _ => FILETYPE_UNKNOWN,
    }
}

pub fn filetype_of_dtype(t: u8) -> u8 {
    match t {
        libc::DT_DIR => FILETYPE_DIRECTORY,
        libc::DT_REG => FILETYPE_REGULAR_FILE,
        libc::DT_LNK => FILETYPE_SYMBOLIC_LINK,
        libc::DT_CHR => FILETYPE_CHARACTER_DEVICE,
        libc::DT_BLK => FILETYPE_BLOCK_DEVICE,
        libc::DT_SOCK => FILETYPE_SOCKET_STREAM,
        _ => FILETYPE_UNKNOWN,
    }
}

// oflags
pub const OFLAGS_CREAT: u16 = 1;
pub const OFLAGS_DIRECTORY: u16 = 2;
pub const OFLAGS_EXCL: u16 = 4;
pub const OFLAGS_TRUNC: u16 = 8;
// fdflags
pub const FDFLAGS_APPEND: u16 = 1;
pub const FDFLAGS_DSYNC: u16 = 2;
pub const FDFLAGS_NONBLOCK: u16 = 4;
pub const FDFLAGS_RSYNC: u16 = 8;
pub const FDFLAGS_SYNC: u16 = 16;
// lookupflags
pub const LOOKUP_SYMLINK_FOLLOW: u32 = 1;
// fstflags
pub const FSTFLAGS_ATIM: u16 = 1;
pub const FSTFLAGS_ATIM_NOW: u16 = 2;
pub const FSTFLAGS_MTIM: u16 = 4;
pub const FSTFLAGS_MTIM_NOW: u16 = 8;
// rights
pub const RIGHT_FD_READ: u64 = 1 << 1;
pub const RIGHT_FD_WRITE: u64 = 1 << 6;
/// Every right defined by preview 1.
pub const RIGHTS_ALL: u64 = (1 << 29) - 1;
// whence
pub const WHENCE_SET: u8 = 0;
pub const WHENCE_CUR: u8 = 1;
pub const WHENCE_END: u8 = 2;
// clocks
pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_PROCESS_CPUTIME: u32 = 2;
pub const CLOCK_THREAD_CPUTIME: u32 = 3;
// poll_oneoff
pub const EVENTTYPE_CLOCK: u8 = 0;
pub const EVENTTYPE_FD_READ: u8 = 1;
pub const EVENTTYPE_FD_WRITE: u8 = 2;
pub const SUBCLOCKFLAGS_ABSTIME: u16 = 1;
