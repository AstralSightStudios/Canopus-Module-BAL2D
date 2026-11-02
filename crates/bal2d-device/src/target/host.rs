//! Module-directory files through the Supervisor's file service.

use alloc::{string::String, vec::Vec};
use core::ffi::{c_char, c_void};

use canopus_abi::{CanopusContext, CanopusFsDirent, CanopusFsStat, ContextV1, FS_CREATE, FS_READ, FS_TRUNC, FS_WRITE};

use crate::module::context;


fn ctx() -> *const CanopusContext {
    context()
}

fn c_path(path: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(path.len() + 1);
    v.extend(path.bytes().filter(|b| *b != 0));
    v.push(0);
    v
}

pub struct Fs;

impl Fs {
    fn obj() -> Option<&'static canopus_abi::CanopusFsObj> {
        let c = ctx();
        if c.is_null() {
            return None;
        }
        unsafe { (*c).fs.as_ref() }
    }

    pub fn stat(path: &str) -> Option<CanopusFsStat> {
        let fs = Self::obj()?;
        let p = c_path(path);
        let mut st = CanopusFsStat { size: 0, is_dir: 0, _pad: [0; 3], mtime: 0 };
        let rc = unsafe { (fs.stat)(ctx() as *const ContextV1, p.as_ptr() as *const c_char, &mut st) };
        (rc == 0).then_some(st)
    }

    pub fn read(path: &str, limit: usize) -> Option<Vec<u8>> {
        let fs = Self::obj()?;
        let p = c_path(path);
        let c = ctx() as *const ContextV1;
        let fd = unsafe { (fs.open)(c, p.as_ptr() as *const c_char, FS_READ) };
        if fd < 0 {
            return None;
        }
        let size = unsafe { (fs.size)(c, fd) };
        let mut out = Vec::new();
        if size > 0 && (size as usize) <= limit {
            out.reserve_exact(size as usize);
        }
        let mut chunk = [0u8; 2048];
        loop {
            let want = chunk.len().min(limit.saturating_sub(out.len()).max(1));
            let n = unsafe { (fs.read)(c, fd, chunk.as_mut_ptr() as *mut c_void, want as u32) };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n as usize]);
            if out.len() >= limit {
                break;
            }
        }
        unsafe { (fs.close)(c, fd) };
        Some(out)
    }

    pub fn mkdirs(path: &str) {
        let Some(fs) = Self::obj() else { return };
        let mut acc = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            let p = c_path(&acc);
            unsafe { (fs.mkdir)(ctx() as *const ContextV1, p.as_ptr() as *const c_char) };
        }
    }

    fn write_raw(path: &str, data: &[u8], flags: u32) -> bool {
        let Some(fs) = Self::obj() else { return false };
        let c = ctx() as *const ContextV1;
        let p = c_path(path);
        let fd = unsafe { (fs.open)(c, p.as_ptr() as *const c_char, flags) };
        if fd < 0 {
            return false;
        }
        let mut written = 0;
        while written < data.len() {
            let n = unsafe { (fs.write)(c, fd, data[written..].as_ptr() as *const c_void, (data.len() - written).min(4096) as u32) };
            if n <= 0 {
                break;
            }
            written += n as usize;
        }
        unsafe {
            (fs.sync)(c, fd);
            (fs.close)(c, fd);
        }
        written == data.len()
    }

    pub fn remove(path: &str) -> bool {
        let Some(fs) = Self::obj() else { return false };
        let p = c_path(path);
        unsafe { (fs.remove)(ctx() as *const ContextV1, p.as_ptr() as *const c_char) == 0 }
    }

    /// Writes `path.tmp`, then renames it over `path`.
    pub fn write(path: &str, data: &[u8]) -> bool {
        if let Some((dir, _)) = path.rsplit_once('/') {
            Self::mkdirs(dir);
        }
        let Some(fs) = Self::obj() else { return false };
        let mut tmp = String::from(path);
        tmp.push_str(".tmp");
        if !Self::write_raw(&tmp, data, FS_WRITE | FS_CREATE | FS_TRUNC) {
            Self::remove(&tmp);
            return false;
        }
        let (from, to) = (c_path(&tmp), c_path(path));
        let c = ctx() as *const ContextV1;
        if unsafe { (fs.rename)(c, from.as_ptr() as *const c_char, to.as_ptr() as *const c_char) } == 0 {
            return true;
        }
        Self::remove(path);
        unsafe { (fs.rename)(c, from.as_ptr() as *const c_char, to.as_ptr() as *const c_char) == 0 }
    }

    pub fn append(path: &str, data: &[u8]) -> bool {
        Self::write_raw(path, data, FS_WRITE | FS_CREATE | canopus_abi::FS_APPEND)
    }

    pub fn list(dir: &str) -> Vec<String> {
        Self::entries(dir).into_iter().map(|(name, _)| name).collect()
    }

    pub fn rename(from: &str, to: &str) -> bool {
        let Some(fs) = Self::obj() else { return false };
        let (a, b) = (c_path(from), c_path(to));
        unsafe { (fs.rename)(ctx() as *const ContextV1, a.as_ptr() as *const c_char, b.as_ptr() as *const c_char) == 0 }
    }

    /// Removes an empty directory (needs the FS service's `rmdir`; false without it).
    pub fn rmdir(path: &str) -> bool {
        canopus_runtime::fs::rmdir(ctx(), path).is_ok()
    }

    /// Every file below `dir` and every directory below it (deepest first), as paths.
    pub fn walk(dir: &str) -> (Vec<String>, Vec<String>) {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut stack = alloc::vec![String::from(dir)];
        while let Some(current) = stack.pop() {
            for (name, is_dir) in Self::entries(&current) {
                let path = alloc::format!("{current}/{name}");
                if is_dir {
                    stack.push(path.clone());
                    dirs.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        // Deepest first, without the generic sort (a few entries at most).
        let mut sorted = Vec::with_capacity(dirs.len());
        while !dirs.is_empty() {
            let i = (0..dirs.len()).max_by_key(|&i| dirs[i].matches('/').count()).unwrap();
            sorted.push(dirs.swap_remove(i));
        }
        let dirs = sorted;
        (files, dirs)
    }

    /// Removes a directory with everything in it (best effort).
    pub fn remove_tree(dir: &str) {
        let (files, dirs) = Self::walk(dir);
        for file in files {
            Self::remove(&file);
        }
        for d in dirs {
            Self::rmdir(&d);
        }
        Self::rmdir(dir);
    }

    pub fn open(path: &str, flags: u32) -> Option<File> {
        let fs = Self::obj()?;
        let p = c_path(path);
        let fd = unsafe { (fs.open)(ctx() as *const ContextV1, p.as_ptr() as *const c_char, flags) };
        (fd >= 0).then_some(File { fd, error: 0 })
    }

    /// Names and kinds (`true` for directories) of a directory's entries.
    pub fn entries(dir: &str) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let Some(fs) = Self::obj() else { return out };
        let c = ctx() as *const ContextV1;
        let p = c_path(dir);
        let handle = unsafe { (fs.opendir)(c, p.as_ptr() as *const c_char) };
        if handle.is_null() {
            return out;
        }
        let mut entry = CanopusFsDirent { name: [0; 64], is_dir: 0, _pad: [0; 3] };
        // 0: an entry, 1: the end, negative: an error.
        while unsafe { (fs.readdir)(c, handle, &mut entry) } == 0 {
            let len = entry.name.iter().position(|b| *b == 0).unwrap_or(entry.name.len());
            let name = String::from_utf8_lossy(&entry.name[..len]).into_owned();
            if name != "." && name != ".." && !name.is_empty() {
                out.push((name, entry.is_dir != 0));
            }
        }
        unsafe { (fs.closedir)(c, handle) };
        out
    }
}

/// An open file of the module directory; closed on drop.
pub struct File {
    fd: i32,
    /// The first error a write met (negative errno), 0 while fine.
    pub error: i32,
}

impl File {
    fn fs() -> &'static canopus_abi::CanopusFsObj {
        Fs::obj().expect("file service")
    }

    /// Reads up to `buf.len()` bytes; 0 at the end or on error.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = unsafe { (Self::fs().read)(ctx() as *const ContextV1, self.fd, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
        n.max(0) as usize
    }

    pub fn seek(&mut self, offset: u32) -> bool {
        unsafe { (Self::fs().seek)(ctx() as *const ContextV1, self.fd, offset as i64, 0) == offset as i64 }
    }

    pub fn read_exact(&mut self, mut out: &mut [u8]) -> bool {
        while !out.is_empty() {
            let n = self.read(out);
            if n == 0 {
                return false;
            }
            out = &mut out[n..];
        }
        true
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> bool {
        while !data.is_empty() {
            let n = unsafe { (Self::fs().write)(ctx() as *const ContextV1, self.fd, data.as_ptr() as *const c_void, data.len().min(4096) as u32) };
            if n <= 0 {
                self.error = if n < 0 { n } else { -5 };
                return false;
            }
            data = &data[n as usize..];
        }
        true
    }

    pub fn sync(&mut self) -> bool {
        unsafe { (Self::fs().sync)(ctx() as *const ContextV1, self.fd) >= 0 }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe { (Self::fs().close)(ctx() as *const ContextV1, self.fd) };
    }
}
