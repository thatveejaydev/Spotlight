//! Thumbnails and basic facts (frame size, duration) of media files for the preview
//! pane. Windows' shell supplies both, the same thumbnails File Explorer shows, read on
//! a background thread so the palette never waits on a slow codec.

use slint::{Rgb8Pixel, SharedPixelBuffer};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, SystemTime},
};

/// Transparent parts of a thumbnail are drawn over a checkerboard of these squares.
const CHECKER_SQUARE: usize = 12;
const CHECKER_LIGHT: u32 = 0xd6;
const CHECKER_DARK: u32 = 0xb4;

/// What Windows knows about one file. Every part is optional: a format without a
/// thumbnail handler still has a size, and an audio file has neither.
pub struct MediaFacts {
    /// The file's [`crate::library::path_key`], as it was requested.
    pub key: String,
    /// When the file was last written, read just before the rest, so a file changed
    /// since can be told apart. None when Windows can't say.
    pub modified: Option<SystemTime>,
    /// The box the thumbnail was fitted into, in pixels.
    pub size: (u32, u32),
    pub thumbnail: Option<SharedPixelBuffer<Rgb8Pixel>>,
    /// Width and height of a video frame or an image, in pixels.
    pub dimensions: Option<(u32, u32)>,
    pub duration: Option<Duration>,
}

/// One file to read, and the box in pixels its thumbnail must fit.
pub struct PreviewRequest {
    pub key: String,
    pub path: PathBuf,
    pub size: (u32, u32),
}

/// Reads [`MediaFacts`] on its own thread. Only the latest request matters: requests
/// passed by while one was being read are skipped.
pub struct MediaPreviewLoader {
    tx: mpsc::Sender<PreviewRequest>,
}

impl MediaPreviewLoader {
    /// Starts the reader thread, which hands each result to `publish`.
    pub fn start(publish: impl Fn(MediaFacts) + Send + 'static) -> Option<Self> {
        let (tx, rx) = mpsc::channel::<PreviewRequest>();
        thread::Builder::new()
            .name("media-preview".into())
            .spawn(move || {
                let _apartment = platform::ComApartment::enter();
                while let Ok(mut request) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        request = newer;
                    }
                    publish(read_facts(request));
                }
            })
            .ok()?;
        Some(Self { tx })
    }

    /// Asks for the facts of a file. False once the thread is gone.
    pub fn request(&self, request: PreviewRequest) -> bool {
        self.tx.send(request).is_ok()
    }
}

/// When the file at `path` was last written, or None when Windows can't say.
pub fn modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

fn read_facts(PreviewRequest { key, path, size }: PreviewRequest) -> MediaFacts {
    let modified = modified_time(&path);
    let thumbnail = platform::thumbnail(&path, size)
        .map(|(width, height, bgra)| flatten(width, height, &bgra));
    let (dimensions, duration) = platform::properties(&path);
    MediaFacts { key, modified, size, thumbnail, dimensions, duration }
}

/// Turns Windows' 32-bit BGRA pixels into opaque RGB, drawing any transparency over a
/// checkerboard. Windows hands back alpha premultiplied for most formats and straight
/// for a few, and leaves it all zero for formats without transparency; all three work.
fn flatten(width: u32, height: u32, bgra: &[u8]) -> SharedPixelBuffer<Rgb8Pixel> {
    let (pixels, _) = bgra.as_chunks::<4>();
    let has_alpha = pixels.iter().any(|&[_, _, _, alpha]| alpha != 0);
    let premultiplied = pixels
        .iter()
        .all(|&[blue, green, red, alpha]| blue <= alpha && green <= alpha && red <= alpha);
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(width, height);
    let columns = width.max(1) as usize;
    for (index, (out, &[blue, green, red, alpha])) in buffer.make_mut_slice().iter_mut().zip(pixels).enumerate() {
        let alpha = u32::from(alpha);
        *out = if !has_alpha || alpha == 255 {
            Rgb8Pixel { r: red, g: green, b: blue }
        } else {
            let (x, y) = (index % columns, index / columns);
            let checker = if (x / CHECKER_SQUARE + y / CHECKER_SQUARE).is_multiple_of(2) {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            let blend = |channel: u8| {
                let channel = u32::from(channel);
                let source = if premultiplied { channel } else { channel * alpha / 255 };
                (source + checker * (255 - alpha) / 255).min(255) as u8
            };
            Rgb8Pixel { r: blend(red), g: blend(green), b: blend(blue) }
        };
    }
    buffer
}

#[cfg(windows)]
mod platform {
    //! The two shell interfaces used, IShellItemImageFactory and IPropertyStore, are
    //! called through hand-written vtables: windows-sys has no COM interfaces.

    use std::{
        ffi::c_void,
        os::windows::ffi::OsStrExt,
        path::Path,
        ptr::null_mut,
        time::Duration,
    };
    use windows_sys::{
        core::{GUID, HRESULT},
        Win32::{
            Foundation::SIZE,
            Graphics::Gdi::{
                DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
                BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
            },
            System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE},
            UI::Shell::{
                PropertiesSystem::{SHGetPropertyStoreFromParsingName, GPS_DEFAULT, PROPERTYKEY},
                SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
            },
        },
    };

    const IID_SHELL_ITEM_IMAGE_FACTORY: GUID = GUID::from_u128(0xbcc18b79_ba16_442f_80c4_8a59c30c463b);
    const IID_PROPERTY_STORE: GUID = GUID::from_u128(0x886d8eeb_8cf2_4446_8d02_cdba1dbdcf99);

    const fn key(fmtid: u128, pid: u32) -> PROPERTYKEY {
        PROPERTYKEY { fmtid: GUID::from_u128(fmtid), pid }
    }
    const VIDEO_FRAME_WIDTH: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 3);
    const VIDEO_FRAME_HEIGHT: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 4);
    const IMAGE_WIDTH: PROPERTYKEY = key(0x6444048f_4c8b_11d1_8b70_080036b11a03, 3);
    const IMAGE_HEIGHT: PROPERTYKEY = key(0x6444048f_4c8b_11d1_8b70_080036b11a03, 4);
    /// In 100-nanosecond units.
    const MEDIA_DURATION: PROPERTYKEY = key(0x64440490_4c8b_11d1_8b70_080036b11a03, 3);

    const VT_UI4: u16 = 19;
    const VT_UI8: u16 = 21;
    /// Windows refuses to hand back thumbnails larger than this anyway.
    const MAX_SIDE: i32 = 4096;

    /// Keeps COM initialized on the reader thread, which the shell needs.
    pub struct ComApartment(bool);

    impl ComApartment {
        pub fn enter() -> Self {
            let result = unsafe {
                CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32)
            };
            Self(result >= 0)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    #[repr(C)]
    struct UnknownVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    #[repr(C)]
    struct ImageFactoryVtbl {
        base: UnknownVtbl,
        get_image: unsafe extern "system" fn(*mut c_void, SIZE, i32, *mut HBITMAP) -> HRESULT,
    }

    #[repr(C)]
    struct PropertyStoreVtbl {
        base: UnknownVtbl,
        get_count: usize,
        get_at: usize,
        get_value: unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *mut PropVariant) -> HRESULT,
        set_value: usize,
        commit: usize,
    }

    /// PROPVARIANT, laid out for the integer values read here. Larger than the real one
    /// on 32-bit Windows, which only leaves room to spare.
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        reserved: [u16; 3],
        value: [u64; 2],
    }

    #[link(name = "ole32")]
    extern "system" {
        fn PropVariantClear(value: *mut PropVariant) -> HRESULT;
    }

    /// An owned COM interface pointer whose vtable starts with `V`.
    struct Com<V>(*mut *const V);

    impl<V> Com<V> {
        fn new(raw: *mut c_void) -> Option<Self> {
            (!raw.is_null()).then(|| Self(raw.cast()))
        }

        fn vtbl(&self) -> &V {
            unsafe { &**self.0 }
        }

        fn raw(&self) -> *mut c_void {
            self.0.cast()
        }
    }

    impl<V> Drop for Com<V> {
        fn drop(&mut self) {
            // Every COM vtable starts with IUnknown's three methods.
            unsafe {
                let vtbl = *(self.0 as *mut *const UnknownVtbl);
                ((*vtbl).release)(self.raw());
            }
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// The file's thumbnail as top-down BGRA rows, scaled to fit within `width` by
    /// `height`. None when Windows has no thumbnail for it, rather than falling back to
    /// the file's icon.
    pub fn thumbnail(path: &Path, (width, height): (u32, u32)) -> Option<(u32, u32, Vec<u8>)> {
        let size = |pixels: u32| pixels.clamp(1, MAX_SIDE as u32) as i32;
        let path = wide(path);
        let mut raw = null_mut();
        let created =
            unsafe { SHCreateItemFromParsingName(path.as_ptr(), null_mut(), &IID_SHELL_ITEM_IMAGE_FACTORY, &mut raw) };
        let factory = Com::<ImageFactoryVtbl>::new(raw).filter(|_| created >= 0)?;
        let mut bitmap: HBITMAP = null_mut();
        let result = unsafe {
            (factory.vtbl().get_image)(
                factory.raw(),
                SIZE { cx: size(width), cy: size(height) },
                SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY,
                &mut bitmap,
            )
        };
        if result < 0 || bitmap.is_null() {
            return None;
        }
        let pixels = unsafe { read_bitmap(bitmap) };
        unsafe { DeleteObject(bitmap) };
        pixels
    }

    unsafe fn read_bitmap(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        let mut info: BITMAP = std::mem::zeroed();
        let size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(bitmap, size, (&mut info as *mut BITMAP).cast()) == 0 {
            return None;
        }
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return None;
        }
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative asks for rows from the top down.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return None;
        }
        let rows = GetDIBits(screen, bitmap, 0, height as u32, pixels.as_mut_ptr().cast(), &mut header, DIB_RGB_COLORS);
        ReleaseDC(null_mut(), screen);
        (rows == height).then_some((width as u32, height as u32, pixels))
    }

    /// The frame size and duration Windows' property handlers report for the file.
    pub fn properties(path: &Path) -> (Option<(u32, u32)>, Option<Duration>) {
        let path = wide(path);
        let mut raw = null_mut();
        let opened = unsafe {
            SHGetPropertyStoreFromParsingName(path.as_ptr(), null_mut(), GPS_DEFAULT, &IID_PROPERTY_STORE, &mut raw)
        };
        let Some(store) = Com::<PropertyStoreVtbl>::new(raw).filter(|_| opened >= 0) else {
            return (None, None);
        };
        let read = |key: &PROPERTYKEY| -> Option<u64> {
            let mut value = PropVariant { vt: 0, reserved: [0; 3], value: [0; 2] };
            let result = unsafe { (store.vtbl().get_value)(store.raw(), key, &mut value) };
            let number = match value.vt {
                _ if result < 0 => None,
                VT_UI4 => Some(value.value[0] & 0xffff_ffff),
                VT_UI8 => Some(value.value[0]),
                _ => None,
            };
            unsafe { PropVariantClear(&mut value) };
            number.filter(|&number| number > 0)
        };
        let pair = |width: &PROPERTYKEY, height: &PROPERTYKEY| {
            Some((u32::try_from(read(width)?).ok()?, u32::try_from(read(height)?).ok()?))
        };
        let dimensions = pair(&VIDEO_FRAME_WIDTH, &VIDEO_FRAME_HEIGHT).or_else(|| pair(&IMAGE_WIDTH, &IMAGE_HEIGHT));
        let duration = read(&MEDIA_DURATION).map(|ticks| Duration::from_nanos(ticks.saturating_mul(100)));
        (dimensions, duration)
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{path::Path, time::Duration};

    pub struct ComApartment;

    impl ComApartment {
        pub fn enter() -> Self {
            Self
        }
    }

    pub fn thumbnail(_path: &Path, _size: (u32, u32)) -> Option<(u32, u32, Vec<u8>)> {
        None
    }

    pub fn properties(_path: &Path) -> (Option<(u32, u32)>, Option<Duration>) {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(buffer: &SharedPixelBuffer<Rgb8Pixel>, index: usize) -> (u8, u8, u8) {
        let pixel = buffer.as_slice()[index];
        (pixel.r, pixel.g, pixel.b)
    }

    #[test]
    fn opaque_thumbnails_keep_their_colors_and_ignore_an_empty_alpha_channel() {
        // BGRA: a red and a blue pixel, with the all-zero alpha of formats without transparency.
        let flat = flatten(2, 1, &[0, 0, 200, 0, 180, 0, 0, 0]);
        assert_eq!(rgb(&flat, 0), (200, 0, 0));
        assert_eq!(rgb(&flat, 1), (0, 0, 180));
    }

    #[test]
    fn transparency_shows_the_checkerboard() {
        // A clear pixel, then half-transparent white, premultiplied as Windows hands it back.
        let flat = flatten(2, 1, &[0, 0, 0, 0, 128, 128, 128, 128]);
        let light = CHECKER_LIGHT as u8;
        assert_eq!(rgb(&flat, 0), (light, light, light));
        let half = (128 + CHECKER_LIGHT * 127 / 255) as u8;
        assert_eq!(rgb(&flat, 1), (half, half, half));

        // The same white with straight alpha is scaled down first.
        let straight = flatten(1, 1, &[255, 255, 255, 128]);
        let blended = (255 * 128 / 255 + CHECKER_LIGHT * 127 / 255) as u8;
        assert_eq!(rgb(&straight, 0), (blended, blended, blended));
    }

    #[test]
    fn checker_squares_alternate() {
        let width = CHECKER_SQUARE * 2;
        let mut clear = vec![0u8; width * 4];
        clear[3] = 1; // One barely visible pixel makes the alpha channel count.
        let flat = flatten(width as u32, 1, &clear);
        assert_eq!(rgb(&flat, CHECKER_SQUARE - 1).0, CHECKER_LIGHT as u8);
        assert_eq!(rgb(&flat, CHECKER_SQUARE).0, CHECKER_DARK as u8);
    }
}
