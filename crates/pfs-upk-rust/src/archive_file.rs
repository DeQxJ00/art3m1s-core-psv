//! Archive input must retain 64-bit offsets on Vita. Its newlib off_t is i32,
//! so std::fs::File::seek truncates offsets above 2 GiB before calling lseek.
#[cfg(not(target_os = "vita"))]
pub(crate) use std::fs::File as ArchiveFile;

#[cfg(target_os = "vita")]
pub(crate) use native::ArchiveFile;

#[cfg(target_os = "vita")]
mod native {
    use std::ffi::{c_char, c_void, CString};
    use std::io::{self, Read, Seek, SeekFrom};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    extern "C" {
        fn sceIoOpen(path: *const c_char, flags: i32, mode: u32) -> i32;
        fn sceIoClose(fd: i32) -> i32;
        fn sceIoRead(fd: i32, out: *mut c_void, size: u32) -> i32;
        fn sceIoLseek(fd: i32, offset: i64, whence: i32) -> i64;
    }

    pub(crate) struct ArchiveFile(i32);

    fn error(operation: &str, result: i32) -> io::Error {
        io::Error::other(format!("{operation}: Vita I/O error 0x{:08x}", result as u32))
    }

    impl ArchiveFile {
        pub(crate) fn open(path: impl AsRef<Path>) -> io::Result<Self> {
            let path = CString::new(path.as_ref().as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in archive path"))?;
            let fd = unsafe { sceIoOpen(path.as_ptr(), 1, 0) }; // SCE_O_RDONLY
            if fd < 0 { Err(error("sceIoOpen", fd)) } else { Ok(Self(fd)) }
        }
    }

    impl Read for ArchiveFile {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if out.is_empty() { return Ok(0); }
            let size = out.len().min(i32::MAX as usize) as u32;
            let n = unsafe { sceIoRead(self.0, out.as_mut_ptr().cast(), size) };
            if n < 0 { Err(error("sceIoRead", n)) } else { Ok(n as usize) }
        }
    }

    impl Seek for ArchiveFile {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let (offset, whence) = match pos {
                SeekFrom::Start(n) => (i64::try_from(n).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "archive offset exceeds i64")
                })?, 0),
                SeekFrom::Current(n) => (n, 1),
                SeekFrom::End(n) => (n, 2),
            };
            let n = unsafe { sceIoLseek(self.0, offset, whence) };
            if n < 0 { Err(error("sceIoLseek", n as i32)) } else { Ok(n as u64) }
        }
    }

    impl Drop for ArchiveFile {
        fn drop(&mut self) { unsafe { sceIoClose(self.0); } }
    }
}
