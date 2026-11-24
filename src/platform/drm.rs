//! DRM/KMS dumb-buffer LCD backend (raw ioctls, no extra crate).
//!
//! The Cardputer Zero panel is a `panel_mipi_dbi` / `drm_mipi_dbi` SPI display (connector `SPI-1`
//! on `/dev/dri/card1`). We create one RGB565 dumb buffer (XRGB8888 if the plane refuses it),
//! scan it out with SETCRTC once, and afterwards only call `DRM_IOCTL_MODE_DIRTYFB` with the
//! bounding box of the pixels that changed: the driver then sends just that window over SPI,
//! and the commit is synchronous, so there is no tearing.
//!
//! The fbdev emulation (`/dev/fb0`) stays alive underneath: we remember the CRTC state we found and
//! hand the LCD back to it whenever an external app runs (those draw through `/dev/fb0`), when we
//! exit, and permanently if any DRM call fails at runtime. When the process dies the kernel's
//! `lastclose` restores the fbdev console on its own.

use super::display::{damage_rect, rgb565_to_xrgb8888, Display, Rect};
use super::fb::Framebuffer;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;

const FOURCC_RGB565: u32 = 0x3631_4752; // 'RG16'
const FOURCC_XRGB8888: u32 = 0x3432_5258; // 'XR24'
/// 8-bit grey (one byte per pixel), what some grey and black-and-white panels take.
const FOURCC_R8: u32 = 0x2020_3852; // 'R8  '
const CONNECTOR_SPI: u32 = 19;
const CONNECTOR_HDMIA: u32 = 11;
const MODE_TYPE_PREFERRED: u32 = 1 << 3;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ModeInfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub typ: u32,
    pub name: [u8; 32],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CardRes {
    fb_ids: u64,
    crtc_ids: u64,
    connector_ids: u64,
    encoder_ids: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    pub mode_valid: u32,
    pub mode: ModeInfo,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Encoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Connector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Version {
    major: i32,
    minor: i32,
    patch: i32,
    name_len: usize,
    name: u64,
    date_len: usize,
    date: u64,
    desc_len: usize,
    desc: u64,
}

/// Drivers of black-and-white panels (Sharp memory LCDs and their kin): Harbour sends them its
/// 1-bit theme, dithered.
pub fn mono_driver(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["sharp", "memory", "ls0", "ssd130", "st7565", "uc1701"].iter().any(|k| n.contains(k))
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DestroyDumb {
    handle: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FbCmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    pad: u32,
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FbDirty {
    fb_id: u32,
    flags: u32,
    color: u32,
    num_clips: u32,
    clips_ptr: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ClipRect {
    x1: u16,
    y1: u16,
    x2: u16,
    y2: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct GetCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Plane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> u64 {
    ((dir << 30) | ((size as u32) << 16) | (0x64 << 8) | nr) as u64
}
const fn iowr<T>(nr: u32) -> u64 {
    ioc(3, nr, std::mem::size_of::<T>())
}
const fn iow<T>(nr: u32) -> u64 {
    ioc(1, nr, std::mem::size_of::<T>())
}

fn ioctl<T>(fd: i32, req: u64, arg: &mut T) -> io::Result<()> {
    loop {
        let r = unsafe { libc::ioctl(fd, req as _, arg as *mut T) };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn ioctl_none(fd: i32, nr: u32) -> io::Result<()> {
    let r = unsafe { libc::ioctl(fd, ioc(0, nr, 0) as _, 0) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// A connector worth driving, with the CRTC that goes with it.
#[derive(Clone)]
pub struct ConnInfo {
    pub connector_id: u32,
    pub connector_type: u32,
    pub connector_type_id: u32,
    pub connection: u32,
    pub crtc_id: u32,
    pub modes: Vec<ModeInfo>,
    /// Physical size (mm), 0 when unknown.
    pub mm: (u32, u32),
}

/// Raw device access shared by the backend and the probe tool.
pub struct Device {
    pub file: File,
}

impl Device {
    pub fn open(path: &str) -> io::Result<Self> {
        Ok(Self { file: OpenOptions::new().read(true).write(true).open(path)? })
    }
    pub fn fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
    pub fn set_client_cap(&self, cap: u64, val: u64) -> io::Result<()> {
        let mut c = GetCap { capability: cap, value: val };
        ioctl(self.fd(), iow::<GetCap>(0x0d), &mut c)
    }
    pub fn cap(&self, cap: u64) -> io::Result<u64> {
        let mut c = GetCap { capability: cap, value: 0 };
        ioctl(self.fd(), iowr::<GetCap>(0x0c), &mut c)?;
        Ok(c.value)
    }
    pub fn set_master(&self) -> io::Result<()> {
        ioctl_none(self.fd(), 0x1e)
    }

    fn resources(&self) -> io::Result<(Vec<u32>, Vec<u32>)> {
        let mut r = CardRes::default();
        ioctl(self.fd(), iowr::<CardRes>(0xa0), &mut r)?;
        let mut crtcs = vec![0u32; r.count_crtcs as usize];
        let mut conns = vec![0u32; r.count_connectors as usize];
        let mut r2 = CardRes {
            crtc_ids: crtcs.as_mut_ptr() as u64,
            connector_ids: conns.as_mut_ptr() as u64,
            count_crtcs: r.count_crtcs,
            count_connectors: r.count_connectors,
            ..Default::default()
        };
        ioctl(self.fd(), iowr::<CardRes>(0xa0), &mut r2)?;
        Ok((crtcs, conns))
    }

    fn encoder(&self, id: u32) -> io::Result<Encoder> {
        let mut e = Encoder { encoder_id: id, ..Default::default() };
        ioctl(self.fd(), iowr::<Encoder>(0xa6), &mut e)?;
        Ok(e)
    }

    pub fn get_crtc(&self, id: u32) -> io::Result<Crtc> {
        let mut c = Crtc { crtc_id: id, ..Default::default() };
        ioctl(self.fd(), iowr::<Crtc>(0xa1), &mut c)?;
        Ok(c)
    }

    /// The kernel driver's name (DRM_IOCTL_VERSION).
    pub fn driver_name(&self) -> io::Result<String> {
        let mut name = vec![0u8; 64];
        let mut v = Version { name_len: name.len(), name: name.as_mut_ptr() as u64, ..Default::default() };
        ioctl(self.fd(), iowr::<Version>(0x00), &mut v)?;
        name.truncate(v.name_len.min(64));
        Ok(String::from_utf8_lossy(&name).into_owned())
    }

    pub fn connectors(&self) -> io::Result<Vec<ConnInfo>> {
        let (crtcs, conns) = self.resources()?;
        let mut out = Vec::new();
        for id in conns {
            let mut c = Connector { connector_id: id, ..Default::default() };
            ioctl(self.fd(), iowr::<Connector>(0xa7), &mut c)?;
            let mut modes = vec![ModeInfo::default(); c.count_modes as usize];
            let mut encs = vec![0u32; c.count_encoders as usize];
            let mut c2 = Connector {
                connector_id: id,
                modes_ptr: modes.as_mut_ptr() as u64,
                count_modes: c.count_modes,
                encoders_ptr: encs.as_mut_ptr() as u64,
                count_encoders: c.count_encoders,
                ..Default::default()
            };
            ioctl(self.fd(), iowr::<Connector>(0xa7), &mut c2)?;
            // CRTC: the one the current encoder drives, else the first one any encoder allows.
            let mut crtc_id = 0;
            if c2.encoder_id != 0 {
                crtc_id = self.encoder(c2.encoder_id).map(|e| e.crtc_id).unwrap_or(0);
            }
            if crtc_id == 0 {
                'outer: for e in &encs {
                    if let Ok(e) = self.encoder(*e) {
                        for (i, cr) in crtcs.iter().enumerate() {
                            if e.possible_crtcs & (1 << i) != 0 {
                                crtc_id = *cr;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            out.push(ConnInfo {
                connector_id: id,
                connector_type: c2.connector_type,
                connector_type_id: c2.connector_type_id,
                connection: c2.connection,
                crtc_id,
                modes,
                mm: (c2.mm_width, c2.mm_height),
            });
        }
        Ok(out)
    }

    /// (plane id, possible_crtcs, current fb, formats as fourcc)
    pub fn planes(&self) -> io::Result<Vec<(u32, u32, u32, Vec<u32>)>> {
        let mut r = PlaneRes::default();
        ioctl(self.fd(), iowr::<PlaneRes>(0xb5), &mut r)?;
        let mut ids = vec![0u32; r.count_planes as usize];
        let mut r2 = PlaneRes { plane_id_ptr: ids.as_mut_ptr() as u64, count_planes: r.count_planes };
        ioctl(self.fd(), iowr::<PlaneRes>(0xb5), &mut r2)?;
        let mut out = Vec::new();
        for id in ids {
            let mut p = Plane { plane_id: id, ..Default::default() };
            ioctl(self.fd(), iowr::<Plane>(0xb6), &mut p)?;
            let mut fmts = vec![0u32; p.count_format_types as usize];
            let mut p2 = Plane { plane_id: id, count_format_types: p.count_format_types, format_type_ptr: fmts.as_mut_ptr() as u64, ..Default::default() };
            ioctl(self.fd(), iowr::<Plane>(0xb6), &mut p2)?;
            out.push((id, p2.possible_crtcs, p2.fb_id, fmts));
        }
        Ok(out)
    }
}

pub fn fourcc_name(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

/// Find `/dev/dri/cardN` that drives the SPI panel: a connected SPI connector, else a connected
/// non-HDMI one with a small mode.
pub fn find_panel() -> io::Result<String> {
    let mut cards: Vec<String> = std::fs::read_dir("/dev/dri")?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card"))
        .map(|n| format!("/dev/dri/{n}"))
        .collect();
    cards.sort();
    let mut fallback = None;
    // Any card with a connected screen (a handheld's DSI panel, a monitor) when no small panel is found.
    let mut any = None;
    for c in cards {
        let Ok(dev) = Device::open(&c) else { continue };
        let Ok(conns) = dev.connectors() else { continue };
        for k in conns {
            if k.connection != 1 || k.modes.is_empty() {
                continue;
            }
            if k.connector_type == CONNECTOR_SPI {
                return Ok(c);
            }
            if k.modes[0].hdisplay <= 640 && fallback.is_none() {
                fallback = Some(c.clone());
            }
            if any.is_none() {
                any = Some(c.clone());
            }
        }
    }
    fallback.or(any).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no panel connector found under /dev/dri"))
}

fn pick_mode(modes: &[ModeInfo]) -> ModeInfo {
    modes.iter().find(|m| m.typ & MODE_TYPE_PREFERRED != 0).or(modes.first()).copied().unwrap_or_default()
}

pub struct DrmDisplay {
    dev: Device,
    path: String,
    fbdev_path: String,
    conn_id: u32,
    crtc_id: u32,
    mode: ModeInfo,
    saved: Crtc,
    width: usize,
    height: usize,
    /// Physical size of the panel (mm), when the driver reports it.
    mm: Option<(u32, u32)>,
    /// The kernel driver's name (`sharp-drm`, `vc4`...).
    driver: String,
    format: u32,
    fb_id: u32,
    handle: u32,
    pitch: usize,
    map: *mut u8,
    map_len: usize,
    last: Vec<u8>,
    valid: bool,
    /// Our buffer is on the CRTC.
    acquired: bool,
    /// The LCD is currently driven through fbdev (external app running, or permanent fallback).
    ext: Option<Framebuffer>,
    permanent_fbdev: bool,
}

// The mapping is only touched from the owning thread.
unsafe impl Send for DrmDisplay {}

impl DrmDisplay {
    pub fn open(path: Option<&str>, fbdev_path: &str) -> io::Result<Self> {
        let path = match path {
            Some(p) => p.to_string(),
            None => find_panel()?,
        };
        let dev = Device::open(&path)?;
        let conns = dev.connectors()?;
        // The built-in panel first (SPI, or a small mode); any connected screen otherwise (a DSI
        // panel of a handheld, an HDMI monitor).
        let usable = |k: &&ConnInfo| k.connection == 1 && !k.modes.is_empty() && k.crtc_id != 0;
        let conn = conns
            .iter()
            .filter(usable)
            .find(|k| k.connector_type == CONNECTOR_SPI || k.modes[0].hdisplay <= 640)
            .or_else(|| conns.iter().filter(usable).find(|k| k.connector_type != CONNECTOR_HDMIA))
            .or_else(|| conns.iter().find(usable))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path}: no usable panel connector")))?;
        let mm = Some(conn.mm);
        let driver = dev.driver_name().unwrap_or_default();
        let mode = pick_mode(&conn.modes);
        let (width, height) = (mode.hdisplay as usize, mode.vdisplay as usize);
        // Becoming master is what lets an unprivileged `video` user set a mode.
        dev.set_master().map_err(|e| io::Error::new(e.kind(), format!("{path}: cannot become DRM master: {e}")))?;
        let saved = dev.get_crtc(conn.crtc_id)?;

        let mut me = Self {
            dev,
            path,
            fbdev_path: fbdev_path.to_string(),
            conn_id: conn.connector_id,
            crtc_id: conn.crtc_id,
            mode,
            saved,
            width,
            height,
            mm,
            driver,
            format: FOURCC_RGB565,
            fb_id: 0,
            handle: 0,
            pitch: 0,
            map: std::ptr::null_mut(),
            map_len: 0,
            last: Vec::new(),
            valid: false,
            acquired: false,
            ext: None,
            permanent_fbdev: false,
        };
        // RGB565 first (no conversion, what the panel wants); XRGB8888 (driver converts) otherwise;
        // 8-bit grey for panels that take only that.
        let mut err = None;
        for (fourcc, bpp) in [(FOURCC_RGB565, 16u32), (FOURCC_XRGB8888, 32), (FOURCC_R8, 8)] {
            match me.create_buffer(fourcc, bpp) {
                Ok(()) => {
                    err = None;
                    break;
                }
                Err(e) => err = Some(e),
            }
        }
        if let Some(e) = err {
            return Err(e);
        }
        Ok(me)
    }

    fn create_buffer(&mut self, fourcc: u32, bpp: u32) -> io::Result<()> {
        let fd = self.dev.fd();
        let mut cd = CreateDumb { width: self.width as u32, height: self.height as u32, bpp, ..Default::default() };
        ioctl(fd, iowr::<CreateDumb>(0xb2), &mut cd)?;
        let mut fb = FbCmd2 {
            width: cd.width,
            height: cd.height,
            pixel_format: fourcc,
            ..Default::default()
        };
        fb.handles[0] = cd.handle;
        fb.pitches[0] = cd.pitch;
        if let Err(e) = ioctl(fd, iowr::<FbCmd2>(0xb8), &mut fb) {
            let mut d = DestroyDumb { handle: cd.handle };
            let _ = ioctl(fd, iowr::<DestroyDumb>(0xb4), &mut d);
            return Err(e);
        }
        let mut md = MapDumb { handle: cd.handle, ..Default::default() };
        let mapped = ioctl(fd, iowr::<MapDumb>(0xb3), &mut md).and_then(|_| {
            let p = unsafe {
                libc::mmap(std::ptr::null_mut(), cd.size as usize, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, md.offset as libc::off_t)
            };
            if p == libc::MAP_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(p as *mut u8)
            }
        });
        match mapped {
            Ok(p) => {
                self.map = p;
                self.map_len = cd.size as usize;
                self.fb_id = fb.fb_id;
                self.handle = cd.handle;
                self.pitch = cd.pitch as usize;
                self.format = fourcc;
                Ok(())
            }
            Err(e) => {
                let mut id = fb.fb_id;
                let _ = ioctl(fd, iowr::<u32>(0xaf), &mut id);
                let mut d = DestroyDumb { handle: cd.handle };
                let _ = ioctl(fd, iowr::<DestroyDumb>(0xb4), &mut d);
                Err(e)
            }
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {}x{} {} (connector {}, crtc {}, fb {})",
            self.path,
            self.width,
            self.height,
            fourcc_name(self.format),
            self.conn_id,
            self.crtc_id,
            self.fb_id
        )
    }

    fn set_crtc(&self, fb_id: u32, mode: Option<&ModeInfo>) -> io::Result<()> {
        let mut conn = self.conn_id;
        let mut c = Crtc {
            set_connectors_ptr: &mut conn as *mut u32 as u64,
            count_connectors: 1,
            crtc_id: self.crtc_id,
            fb_id,
            mode_valid: 1,
            mode: *mode.unwrap_or(&self.mode),
            ..Default::default()
        };
        ioctl(self.dev.fd(), iowr::<Crtc>(0xa2), &mut c)
    }

    fn dirty(&self, r: Rect) -> io::Result<()> {
        let mut clip = ClipRect { x1: r.x0 as u16, y1: r.y0 as u16, x2: r.x1 as u16, y2: r.y1 as u16 };
        let mut d = FbDirty { fb_id: self.fb_id, num_clips: 1, clips_ptr: &mut clip as *mut ClipRect as u64, ..Default::default() };
        ioctl(self.dev.fd(), iowr::<FbDirty>(0xb1), &mut d)
    }

    fn bpp_bytes(&self) -> usize {
        match self.format {
            FOURCC_RGB565 => 2,
            FOURCC_R8 => 1,
            _ => 4,
        }
    }

    fn blit(&mut self, px: &[u8], r: Rect) {
        let row = self.width * 2;
        for y in r.y0..r.y1 {
            let src = &px[y * row + r.x0 * 2..y * row + r.x1 * 2];
            let off = y * self.pitch + r.x0 * self.bpp_bytes();
            debug_assert!(off + r.width() * self.bpp_bytes() <= self.map_len);
            unsafe {
                let dst = self.map.add(off);
                if self.format == FOURCC_RGB565 {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                } else if self.format == FOURCC_R8 {
                    for (i, p) in src.chunks_exact(2).enumerate() {
                        dst.add(i).write(super::display::luma565(p[0], p[1]));
                    }
                } else {
                    let dst = dst as *mut u32;
                    for (i, p) in src.chunks_exact(2).enumerate() {
                        dst.add(i).write(rgb565_to_xrgb8888(p[0], p[1]));
                    }
                }
            }
        }
    }

    /// Give the LCD back to the fbdev emulation, showing `frame` (our last picture) on it first so
    /// nothing stale flashes. Used for external apps, on drop, and as the runtime fallback.
    fn to_fbdev(&mut self, frame: Option<&[u8]>) -> io::Result<()> {
        if self.ext.is_some() {
            return Ok(());
        }
        let mut fb = Framebuffer::open(&self.fbdev_path)?;
        if let Some(f) = frame {
            // fbdev's own buffer is not on the CRTC yet, so this does not hit the SPI bus.
            let _ = fb.present(f);
        }
        if self.acquired {
            let _ = self.set_crtc(self.saved.fb_id, if self.saved.mode_valid != 0 { Some(&self.saved.mode) } else { None });
            self.acquired = false;
        }
        self.ext = Some(fb);
        Ok(())
    }

    fn degrade(&mut self, px: &[u8], why: &io::Error) -> io::Result<usize> {
        eprintln!("[display] DRM failed ({why}); falling back to fbdev {}", self.fbdev_path);
        self.permanent_fbdev = true;
        self.to_fbdev(Some(px))?;
        self.ext.as_mut().unwrap().present(px)
    }
}

impl Display for DrmDisplay {
    fn present(&mut self, px: &[u8]) -> io::Result<usize> {
        let row = self.width * 2;
        if px.len() < row * self.height {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too small"));
        }
        if let Some(e) = self.ext.as_mut() {
            return e.present(px);
        }
        let rect = if self.valid && self.acquired {
            match damage_rect(&self.last, px, self.width, self.height) {
                Some(r) => r,
                None => return Ok(0),
            }
        } else {
            Rect::full(self.width, self.height)
        };
        self.blit(px, rect);
        let res = if self.acquired { self.dirty(rect) } else { self.set_crtc(self.fb_id, None).map(|_| self.acquired = true) };
        if let Err(e) = res {
            return self.degrade(px, &e);
        }
        self.last.clear();
        self.last.extend_from_slice(&px[..row * self.height]);
        self.valid = true;
        Ok(rect.area() * 2)
    }

    fn invalidate(&mut self) {
        self.valid = false;
        if let Some(e) = self.ext.as_mut() {
            e.invalidate();
        }
    }

    fn taken_over(&self) -> bool {
        self.ext.as_ref().map_or(false, |e| e.taken_over())
    }

    fn enter_external(&mut self) {
        let last = if self.valid { Some(self.last.clone()) } else { None };
        if let Err(e) = self.to_fbdev(last.as_deref()) {
            eprintln!("[display] cannot hand the LCD to fbdev: {e}");
        }
    }

    fn leave_external(&mut self) {
        if !self.permanent_fbdev {
            self.ext = None; // next present re-acquires and rewrites the whole frame
            self.valid = false;
        } else if let Some(e) = self.ext.as_mut() {
            e.invalidate();
        }
    }

    fn wants_refresh(&self) -> bool {
        self.ext.is_some()
    }

    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn mm(&self) -> Option<(u32, u32)> {
        self.mm.filter(|(w, h)| *w > 0 && *h > 0)
    }
    fn kind(&self) -> super::display::PanelKind {
        use super::display::PanelKind;
        if mono_driver(&self.driver) {
            PanelKind::Mono
        } else if self.format == FOURCC_R8 {
            PanelKind::Gray
        } else {
            PanelKind::Color
        }
    }
    fn backend(&self) -> &'static str {
        if self.ext.is_some() {
            "drm+fbdev"
        } else {
            "drm"
        }
    }
}

impl Drop for DrmDisplay {
    fn drop(&mut self) {
        if self.acquired {
            let last = if self.valid { Some(self.last.clone()) } else { None };
            let _ = self.to_fbdev(last.as_deref());
        }
        let fd = self.dev.fd();
        unsafe {
            if !self.map.is_null() {
                libc::munmap(self.map as *mut libc::c_void, self.map_len);
            }
        }
        let mut id = self.fb_id;
        let _ = ioctl(fd, iowr::<u32>(0xaf), &mut id);
        let mut d = DestroyDumb { handle: self.handle };
        let _ = ioctl(fd, iowr::<DestroyDumb>(0xb4), &mut d);
    }
}
