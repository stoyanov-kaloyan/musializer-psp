use core::ffi::c_void;

use alloc::vec::Vec;
use psp::sys::{self, SceUid};

pub struct AssetStream {
    fd: SceUid,
    path_z: Vec<u8>,
}

impl AssetStream {
    pub fn open(path: &str) -> Result<Self, i32> {
        let path_z = to_c_path(path);
        let fd = unsafe { sys::sceIoOpen(path_z.as_ptr(), sys::IoOpenFlags::RD_ONLY, 0) };
        if fd.0 < 0 {
            Err(fd.0)
        } else {
            Ok(Self { fd, path_z })
        }
    }

    /// Read up to out.len() bytes into out
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, i32> {
        let r =
            unsafe { sys::sceIoRead(self.fd, out.as_mut_ptr() as *mut c_void, out.len() as u32) };
        if r < 0 { Err(r) } else { Ok(r as usize) }
    }

    pub fn seek(&mut self, offset: i64, whence: sys::IoWhence) -> Result<i64, i32> {
        let pos = unsafe { sys::sceIoLseek(self.fd, offset, whence) };
        if pos < 0 { Err(pos as i32) } else { Ok(pos) }
    }

    /// Get file size without changing the current position.
    pub fn size(&mut self) -> Result<i64, i32> {
        let cur = self.seek(0, sys::IoWhence::Cur)?;
        let end = self.seek(0, sys::IoWhence::End)?;
        let _ = self.seek(cur, sys::IoWhence::Set)?;
        Ok(end)
    }
}

impl Drop for AssetStream {
    fn drop(&mut self) {
        unsafe { sys::sceIoClose(self.fd) };
    }
}

pub fn to_c_path(path: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(path.len() + 1);
    v.extend_from_slice(path.as_bytes());
    v.push(0);
    v
}

/// Load assets from storage(to avoid bloating the binary)
/// will replace include_bytes! usage
/// load_asset should ideally be replaced with `AssetStream` for larger files
pub fn load_asset(path: &str, buffer: &mut [u8]) -> Option<usize> {
    let c_path = to_c_path(path);

    let fd = unsafe { sys::sceIoOpen(c_path.as_ptr(), sys::IoOpenFlags::RD_ONLY, 0) };
    if fd.0 < 0 {
        return None;
    }

    let mut total = 0usize;

    while total < buffer.len() {
        let remaining = (buffer.len() - total) as u32;

        let r =
            unsafe { sys::sceIoRead(fd, buffer[total..].as_mut_ptr() as *mut c_void, remaining) };

        if r < 0 {
            // read error
            unsafe { sys::sceIoClose(fd) };
            return None;
        }
        if r == 0 {
            // EOF
            break;
        }

        total += r as usize;
    }

    unsafe { sys::sceIoClose(fd) };
    Some(total)
}

/// round up to the next power of 2 (for texture dimensions)
pub fn next_power_of_2(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut v = n - 1;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    v + 1
}

/// Decode PNG data into the provided buffer and return a slice of pixels
pub fn decode_png_into<'a>(png: &[u8], buffer: &'a mut [u8]) -> (&'a [u8], usize, usize) {
    let mut image = minipng::decode_png(png, buffer).expect("bad PNG");
    let _ = image.convert_to_rgba8bpc();

    let w = image.width() as usize;
    let h = image.height() as usize;
    let len = w.checked_mul(h).and_then(|v| v.checked_mul(4)).unwrap_or(0);

    let pixels: &'a [u8] = &buffer[..len];
    (pixels, w, h)
}
