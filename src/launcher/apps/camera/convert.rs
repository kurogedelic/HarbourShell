//! Pixel work for the Camera app: frames as they come from the device, conversion to
//! grey for the preview and to RGB for the saved photo, MJPEG decoding. No hardware here,
//! so everything is testable.


/// A frame as the device delivered it.
pub enum Raw {
    /// Packed YUV 4:2:2 (`Y0 U Y1 V`), `w * h * 2` bytes.
    Yuyv { w: u32, h: u32, data: Vec<u8> },
    /// One JPEG image (UVC "MJPG" frames).
    Mjpeg(Vec<u8>),
    /// Tightly packed RGB (mock source).
    Rgb { w: u32, h: u32, data: Vec<u8> },
}

/// A frame the app can keep for the photo (YUYV is converted to RGB only when saved).
pub enum Still {
    Rgb { w: u32, h: u32, data: Vec<u8> },
    Yuyv { w: u32, h: u32, data: Vec<u8> },
}

impl Still {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Still::Rgb { w, h, .. } | Still::Yuyv { w, h, .. } => (*w, *h),
        }
    }

    /// Full-resolution RGB for saving.
    pub fn to_rgb(&self) -> (u32, u32, Vec<u8>) {
        match self {
            Still::Rgb { w, h, data } => (*w, *h, data.clone()),
            Still::Yuyv { w, h, data } => (*w, *h, yuyv_to_rgb(data, *w as usize, *h as usize)),
        }
    }
}

const STD_DHT: &[u8] = include_bytes!("std_dht.bin");

/// Many UVC cameras leave the Huffman tables out of their MJPEG frames (the "abbreviated"
/// stream every decoder is expected to complete with the standard tables). Insert them
/// before the start-of-frame marker when the frame has none.
pub fn with_huffman_tables(jpeg: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return None;
    }
    let (mut i, mut sof) = (2, None);
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        match jpeg[i + 1] {
            0xC4 => return Some(std::borrow::Cow::Borrowed(jpeg)), // has its own tables
            0xDA | 0xD9 => break,
            0xD8 | 0x01 | 0xD0..=0xD7 => i += 2,
            m => {
                if matches!(m, 0xC0..=0xC2) && sof.is_none() {
                    sof = Some(i);
                }
                i += 2 + ((jpeg[i + 2] as usize) << 8 | jpeg[i + 3] as usize);
            }
        }
    }
    match sof {
        Some(at) => {
            let mut v = Vec::with_capacity(jpeg.len() + STD_DHT.len());
            v.extend_from_slice(&jpeg[..at]);
            v.extend_from_slice(STD_DHT);
            v.extend_from_slice(&jpeg[at..]);
            Some(std::borrow::Cow::Owned(v))
        }
        None => Some(std::borrow::Cow::Borrowed(jpeg)),
    }
}

/// Decode a JPEG to tightly packed RGB through SDL2_image (the caller has initialised JPG).
pub fn decode_jpeg(jpeg: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let data = with_huffman_tables(jpeg).ok_or("not a JPEG frame")?;
    let (out, w, h) = crate::imgload::decode_rgb24(&data)?;
    Ok((w as u32, h as u32, out))
}

/// Raw frame to something the app keeps.
pub fn to_still(raw: Raw) -> Result<Still, String> {
    match raw {
        Raw::Yuyv { w, h, data } => {
            if data.len() < (w * h * 2) as usize {
                return Err(format!("short YUYV frame ({} bytes for {w}x{h})", data.len()));
            }
            Ok(Still::Yuyv { w, h, data })
        }
        Raw::Rgb { w, h, data } => Ok(Still::Rgb { w, h, data }),
        Raw::Mjpeg(d) => {
            let (w, h, data) = decode_jpeg(&d)?;
            Ok(Still::Rgb { w, h, data })
        }
    }
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BT.601 limited-range YUV to RGB (what UVC cameras send).
pub fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = (y as i32 - 16).max(0) * 298;
    let (d, e) = (u as i32 - 128, v as i32 - 128);
    [clamp8((c + 409 * e + 128) >> 8), clamp8((c - 100 * d - 208 * e + 128) >> 8), clamp8((c + 516 * d + 128) >> 8)]
}

pub fn yuyv_to_rgb(data: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3);
    for px in data[..w * h * 2].chunks_exact(4) {
        out.extend(yuv_to_rgb(px[0], px[1], px[3]));
        out.extend(yuv_to_rgb(px[2], px[1], px[3]));
    }
    out
}

/// Luma (0..=255, full range) of the YUYV pixel at (x, y).
pub fn yuyv_luma(data: &[u8], w: usize, x: usize, y: usize) -> u8 {
    let y8 = data[(y * w + x) * 2] as i32;
    clamp8(((y8 - 16) * 298 + 128) >> 8)
}

/// Largest size with the picture's aspect ratio inside `aw` x `ah`.
pub fn fit(w: u32, h: u32, aw: u32, ah: u32) -> (u32, u32) {
    crate::launcher::apps::imageviewer::fit(w, h, aw, ah)
}

/// Grey preview of `still` scaled to fit `aw` x `ah`: box average of up to 3 x 3 samples per
/// output pixel (enough to avoid shimmering, cheap for the CM0). Returns (w, h, grey).
pub fn gray_fit(still: &Still, aw: u32, ah: u32) -> (u32, u32, Vec<u8>) {
    let (sw, sh) = still.size();
    let (dw, dh) = fit(sw, sh, aw, ah);
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh];
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        let ystep = ((y1 - y0) / 3).max(1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let xstep = ((x1 - x0) / 3).max(1);
            let (mut acc, mut n) = (0u32, 0u32);
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    acc += match still {
                        Still::Rgb { data, .. } => {
                            let i = (y * sw + x) * 3;
                            crate::launcher::apps::imageviewer::luma(data[i], data[i + 1], data[i + 2]) as u32
                        }
                        Still::Yuyv { data, .. } => yuyv_luma(data, sw, x, y) as u32,
                    };
                    n += 1;
                    x += xstep;
                }
                y += ystep;
            }
            out[dy * dw + dx] = (acc / n.max(1)) as u8;
        }
    }
    (dw as u32, dh as u32, out)
}

/// An animated test pattern (mock mode): grey ramp, checker, moving disc and a sweeping bar.
pub fn test_pattern(w: u32, h: u32, t: u32) -> Vec<u8> {
    let (wi, hi) = (w as i32, h as i32);
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    let (cx, cy) = (wi / 2 + ((t as f32 * 0.11).sin() * wi as f32 * 0.3) as i32, hi / 2 + ((t as f32 * 0.17).cos() * hi as f32 * 0.25) as i32);
    let r = hi / 5;
    let bar = (t as i32 * 7) % wi;
    for y in 0..hi {
        for x in 0..wi {
            let mut v = if y < hi / 4 {
                (x * 255 / wi.max(1)) as u8 // ramp
            } else if ((x / (wi / 16).max(1)) + (y / (hi / 12).max(1))) % 2 == 0 {
                200
            } else {
                60
            };
            let (dx, dy) = (x - cx, y - cy);
            if dx * dx + dy * dy < r * r {
                v = 245 - ((dx * dx + dy * dy) * 160 / (r * r).max(1)) as u8;
            }
            if (x - bar).abs() < 3 {
                v = 255 - v;
            }
            out.extend([v, v, v]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/cam_32x24.jpg");

    fn init_jpg() {}

    #[test]
    fn yuyv_luma_maps_limited_range_to_full() {
        let w = 2;
        let black = [16, 128, 16, 128];
        let white = [235, 128, 235, 128];
        assert_eq!(yuyv_luma(&black, w, 0, 0), 0);
        assert!(yuyv_luma(&white, w, 1, 0) >= 254);
        assert_eq!(yuyv_luma(&[16, 128, 126, 128], w, 1, 0), 128);
    }

    #[test]
    fn yuyv_converts_to_rgb() {
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        let w = yuv_to_rgb(235, 128, 128);
        assert!(w.iter().all(|&c| c >= 254));
        let red = yuv_to_rgb(81, 90, 240);
        assert!(red[0] > 240 && red[1] < 20 && red[2] < 20, "{red:?}");
        let rgb = yuyv_to_rgb(&[16, 128, 235, 128, 81, 90, 81, 240], 4, 1);
        assert_eq!(rgb.len(), 12);
        assert_eq!(&rgb[0..3], &[0, 0, 0]);
    }

    #[test]
    fn gray_fit_scales_and_keeps_aspect() {
        let w = 64;
        let mut data = Vec::new();
        for _y in 0..48 {
            for x in 0..w {
                data.extend([if x < 32 { 16u8 } else { 235 }, 128]);
            }
        }
        let still = Still::Yuyv { w: 64, h: 48, data };
        let (dw, dh, g) = gray_fit(&still, 32, 32);
        assert_eq!((dw, dh), (32, 24));
        assert_eq!(g[0], 0);
        assert!(g[31] >= 254);
        let rgb = Still::Rgb { w: 2, h: 1, data: vec![255, 255, 255, 0, 0, 0] };
        assert_eq!(gray_fit(&rgb, 2, 1).2, vec![255, 0]);
    }

    #[test]
    fn mjpeg_fixture_decodes() {
        init_jpg();
        let (w, h, rgb) = decode_jpeg(FIXTURE).expect("decode");
        assert_eq!((w, h), (32, 24));
        let px = |x: usize, y: usize| &rgb[(y * 32 + x) * 3..(y * 32 + x) * 3 + 3];
        assert!(px(4, 12)[0] > 200 && px(4, 12)[2] < 60, "left is red: {:?}", px(4, 12));
        assert!(px(28, 12)[2] > 200 && px(28, 12)[0] < 60, "right is blue");
    }

    #[test]
    fn frames_without_huffman_tables_are_completed() {
        init_jpg();
        // Strip the DHT segments like a cheap UVC camera does.
        let mut bare = vec![0xFF, 0xD8];
        let mut i = 2;
        while i + 4 <= FIXTURE.len() {
            let m = FIXTURE[i + 1];
            let l = (FIXTURE[i + 2] as usize) << 8 | FIXTURE[i + 3] as usize;
            if m == 0xDA {
                bare.extend_from_slice(&FIXTURE[i..]);
                break;
            }
            if m != 0xC4 {
                bare.extend_from_slice(&FIXTURE[i..i + 2 + l]);
            }
            i += 2 + l;
        }
        assert!(bare.len() < FIXTURE.len() - 400);
        let (w, h, rgb) = decode_jpeg(&bare).expect("decode abbreviated");
        assert_eq!((w, h), (32, 24));
        assert!(rgb[(12 * 32 + 4) * 3] > 200);
        assert!(decode_jpeg(b"junk").is_err());
        assert!(matches!(with_huffman_tables(FIXTURE).unwrap(), std::borrow::Cow::Borrowed(_)), "complete frames pass untouched");
    }

    #[test]
    fn test_pattern_moves() {
        let a = test_pattern(64, 48, 0);
        let b = test_pattern(64, 48, 20);
        assert_eq!(a.len(), 64 * 48 * 3);
        assert_ne!(a, b);
    }
}
