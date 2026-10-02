//! Executable memory.
//!
//! On macOS the region is mapped with `MAP_JIT` and written while the calling thread has
//! JIT write protection turned off (`pthread_jit_write_protect_np(0)`); the pages are never
//! writable and executable for the same thread at once. On Linux the pages are mapped
//! writable, filled, then switched to read+execute with `mprotect`. Either way the
//! instruction cache is invalidated before the code runs.

use std::ptr;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_jit_write_protect_np(enabled: libc::c_int);
    fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
}

pub struct CodeMemory {
    ptr: *mut u8,
    len: usize,
}

// The code is immutable once published.
unsafe impl Send for CodeMemory {}
unsafe impl Sync for CodeMemory {}

impl CodeMemory {
    /// Copy `code` into fresh executable memory.
    pub fn new(code: &[u8]) -> Result<CodeMemory, String> {
        let page = 16384;
        let len = code.len().max(4).div_ceil(page) * page;
        unsafe {
            #[cfg(target_os = "macos")]
            let p = libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            );
            #[cfg(not(target_os = "macos"))]
            let p = libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(format!(
                    "mmap of {len} bytes of code failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let p = p as *mut u8;
            #[cfg(target_os = "macos")]
            {
                pthread_jit_write_protect_np(0);
                ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
                pthread_jit_write_protect_np(1);
                sys_icache_invalidate(p as *mut libc::c_void, code.len());
            }
            #[cfg(not(target_os = "macos"))]
            {
                ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
                if libc::mprotect(
                    p as *mut libc::c_void,
                    len,
                    libc::PROT_READ | libc::PROT_EXEC,
                ) != 0
                {
                    libc::munmap(p as *mut libc::c_void, len);
                    return Err("mprotect of code failed".into());
                }
                #[cfg(target_os = "linux")]
                __clear_cache(
                    p as *mut libc::c_char,
                    p.add(code.len()) as *mut libc::c_char,
                );
            }
            Ok(CodeMemory { ptr: p, len })
        }
    }

    pub fn ptr(&self) -> *const u8 {
        self.ptr
    }
}

impl Drop for CodeMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}
