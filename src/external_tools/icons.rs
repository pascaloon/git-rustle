//! Executable resource icons. Call only on a background worker, never in render.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::SystemTime,
};
type Key = (PathBuf, u64, Option<SystemTime>);
type Cache = HashMap<Key, Option<Arc<gpui::Image>>>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

pub fn load(path: &Path) -> Option<Arc<gpui::Image>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file()
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    let key = (path.to_path_buf(), metadata.len(), metadata.modified().ok());
    let mut cache = CACHE.lock().unwrap();
    if let Some(image) = cache.get(&key) {
        return image.clone();
    }
    let image =
        extract(path).map(|bytes| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Bmp, bytes)));
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(key, image.clone());
    image
}

#[cfg(windows)]
fn extract(path: &Path) -> Option<Vec<u8>> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};
    type Handle = *mut c_void;
    #[repr(C)]
    struct Header {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bits: u16,
        compression: u32,
        image_size: u32,
        x: i32,
        y: i32,
        used: u32,
        important: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrivateExtractIconsW(
            file: *const u16,
            index: i32,
            width: i32,
            height: i32,
            icons: *mut Handle,
            ids: *mut u32,
            count: u32,
            flags: u32,
        ) -> u32;
        fn DestroyIcon(icon: Handle) -> i32;
        fn DrawIconEx(
            dc: Handle,
            x: i32,
            y: i32,
            icon: Handle,
            width: i32,
            height: i32,
            step: u32,
            brush: Handle,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: Handle) -> Handle;
        fn CreateDIBSection(
            dc: Handle,
            info: *const Header,
            usage: u32,
            bits: *mut *mut c_void,
            section: Handle,
            offset: u32,
        ) -> Handle;
        fn SelectObject(dc: Handle, object: Handle) -> Handle;
        fn DeleteObject(object: Handle) -> i32;
        fn DeleteDC(dc: Handle) -> i32;
    }
    const SIZE: usize = 32;
    unsafe fn draw(icon: Handle, background: u8) -> Option<Vec<u8>> {
        // Memory DC and our own DIB only; no window/screen DC or screenshot APIs.
        let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if dc.is_null() {
            return None;
        }
        let header = Header {
            size: 40,
            width: SIZE as i32,
            height: -(SIZE as i32),
            planes: 1,
            bits: 32,
            compression: 0,
            image_size: (SIZE * SIZE * 4) as u32,
            x: 0,
            y: 0,
            used: 0,
            important: 0,
        };
        let mut pixels = ptr::null_mut();
        let bitmap = unsafe { CreateDIBSection(dc, &header, 0, &mut pixels, ptr::null_mut(), 0) };
        if bitmap.is_null() {
            unsafe { DeleteDC(dc) };
            return None;
        }
        let previous = unsafe { SelectObject(dc, bitmap) };
        let buffer =
            unsafe { std::slice::from_raw_parts_mut(pixels.cast::<u8>(), SIZE * SIZE * 4) };
        buffer.fill(background);
        let ok = unsafe {
            DrawIconEx(
                dc,
                0,
                0,
                icon,
                SIZE as i32,
                SIZE as i32,
                0,
                ptr::null_mut(),
                3,
            )
        } != 0;
        let result = ok.then(|| buffer.to_vec());
        unsafe {
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
        }
        result
    }
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut icon = ptr::null_mut();
    // Resource extraction reads the executable; it never launches the editor.
    if unsafe { PrivateExtractIconsW(file.as_ptr(), 0, 32, 32, &mut icon, ptr::null_mut(), 1, 0) }
        != 1
        || icon.is_null()
    {
        return None;
    }
    let pair = unsafe { (draw(icon, 0), draw(icon, 255)) };
    unsafe { DestroyIcon(icon) };
    let (Some(mut black), Some(white)) = pair else {
        return None;
    };
    let mut visible = false;
    for (pixel, white) in black.chunks_exact_mut(4).zip(white.chunks_exact(4)) {
        let difference = (0..3)
            .map(|i| white[i].saturating_sub(pixel[i]))
            .max()
            .unwrap();
        let alpha = 255 - difference;
        for channel in &mut pixel[..3] {
            *channel = if alpha == 0 {
                0
            } else {
                ((u32::from(*channel) * 255) / u32::from(alpha)).min(255) as u8
            };
        }
        pixel[3] = alpha;
        visible |= alpha > 0;
    }
    if !visible {
        return None;
    }
    // BMP V4 with explicit RGBA masks preserves transparency in GPUI's decoder.
    let mut bmp = vec![0u8; 122];
    bmp[..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&((122 + black.len()) as u32).to_le_bytes());
    bmp[10..14].copy_from_slice(&122u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&108u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&32i32.to_le_bytes());
    bmp[22..26].copy_from_slice(&(-32i32).to_le_bytes());
    bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
    bmp[30..34].copy_from_slice(&3u32.to_le_bytes());
    bmp[34..38].copy_from_slice(&(black.len() as u32).to_le_bytes());
    for (offset, mask) in [
        (54, 0x00ff0000u32),
        (58, 0x0000ff00),
        (62, 0x000000ff),
        (66, 0xff000000),
    ] {
        bmp[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
    }
    bmp[70..74].copy_from_slice(&0x73524742u32.to_le_bytes());
    bmp.extend(black);
    Some(bmp)
}

#[cfg(not(windows))]
fn extract(_: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_real_executable_resource_once_and_reuses_the_same_image() {
        let executable = std::env::current_exe().unwrap();
        let first = load(&executable).expect("test executable has the application icon resource");
        let second = load(&executable).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        // The isolated native scenario verifies GPUI decoding and painted bounds.
        assert_eq!(&first.bytes()[..2], b"BM");
        assert!(
            first.bytes()[122..]
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0)
        );
        assert!(load(&executable.with_extension("missing.exe")).is_none());
        let fixture = tempfile::tempdir().unwrap();
        let no_icon = fixture.path().join("not-a-resource-executable.exe");
        std::fs::write(&no_icon, b"synthetic missing-resource fixture").unwrap();
        assert!(load(&no_icon).is_none());
        std::fs::copy(&executable, &no_icon).unwrap();
        assert!(
            load(&no_icon).is_some(),
            "replacing an executable must invalidate a cached missing icon"
        );
    }

    #[test]
    #[ignore = "one warmup plus 15 executable-resource extraction and icon cache samples"]
    fn benchmark_executable_icons() {
        let executable = std::env::var_os("GIT_RUSTLE_ICON_BENCH_EXE")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_exe().unwrap());
        let cached = load(&executable).expect("benchmark executable icon");
        for sample in -1..15 {
            let start = std::time::Instant::now();
            let image = extract(&executable).expect("real resource extraction");
            let extraction_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(!image.is_empty());
            let start = std::time::Instant::now();
            for _ in 0..1000 {
                assert!(Arc::ptr_eq(
                    &cached,
                    &load(std::hint::black_box(&executable)).unwrap()
                ));
            }
            let cache_hit_ms = start.elapsed().as_secs_f64();
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"extraction_ms":extraction_ms,"cache_hit_ms":cache_hit_ms})
                );
            }
        }
    }
}
