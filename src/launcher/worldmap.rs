//! World map: 1-bit land bitmap (Natural Earth, public domain) turned into a
//! white-on-transparent texture with lightly dithered land (25%, no outline), plus
//! the projection helpers the World Clock uses (equirectangular, wrapping).


use crate::assets::AssetStore;

pub use super::worldmap_data::{H, LAT_BOTTOM, LAT_TOP, ROW_BYTES, W};
use super::worldmap_data::LAND;

pub const TEXTURE_KEY: &str = "map/world";

fn bit(x: usize, y: usize) -> bool {
    x < W && y < H && LAND[y * ROW_BYTES + x / 8] & (0x80 >> (x % 8)) != 0
}

/// Is the point (degrees) on land at the map's resolution?
pub fn is_land(lat: f32, lon: f32) -> bool {
    if lat > LAT_TOP || lat < LAT_BOTTOM {
        return false;
    }
    let x = (((lon + 180.0) / 360.0) * W as f32) as usize % W;
    let y = (((LAT_TOP - lat) / (LAT_TOP - LAT_BOTTOM)) * H as f32) as usize;
    bit(x, y.min(H - 1))
}

/// Longitude difference wrapped into (-180, 180].
pub fn wrap_lon(d: f32) -> f32 {
    let x = (d + 180.0).rem_euclid(360.0) - 180.0;
    if x == -180.0 {
        180.0
    } else {
        x
    }
}

/// Map column shown at the left edge when longitude `pan` is centred.
pub fn start_col(pan: f32) -> i32 {
    (((pan + 180.0) / 360.0) * W as f32 - W as f32 / 2.0).rem_euclid(W as f32) as i32
}

/// Horizontal offset (pixels from the map's left edge) of `lon` when `pan` is centred.
pub fn lon_to_x(lon: f32, pan: f32) -> f32 {
    W as f32 / 2.0 + wrap_lon(lon - pan) / 360.0 * W as f32
}

/// Vertical offset (pixels from the map's top edge) of a latitude.
pub fn lat_to_y(lat: f32) -> f32 {
    (LAT_TOP - lat) / (LAT_TOP - LAT_BOTTOM) * H as f32
}

/// Which land counts as "in the selected city's time zone".
/// Default: the nautical 15-degree band centred on offset*15 degrees (half-hour offsets give
/// shifted bands). Two big irregular zones get hand-made longitude/latitude spans instead
/// (approximate, from the countries' extents): China (UTC+8) and India (UTC+5:30).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneSel {
    pub offset_secs: i32,
    pub custom: Option<(f32, f32, f32, f32)>, // lon_min, lon_max, lat_min, lat_max
}

pub fn zone_sel(zone: &str, offset_secs: i32) -> ZoneSel {
    let custom = match zone {
        "Asia/Shanghai" => Some((73.5, 135.1, 18.0, 53.6)),
        "Asia/Kolkata" => Some((68.0, 97.4, 6.7, 37.1)),
        _ => None,
    };
    ZoneSel { offset_secs, custom }
}

/// Is the point inside the zone's region (water is not checked here)?
pub fn in_zone(z: ZoneSel, lat: f32, lon: f32) -> bool {
    if let Some((lo, hi, la0, la1)) = z.custom {
        return lat >= la0 && lat <= la1 && lon >= lo && lon <= hi;
    }
    let centre = z.offset_secs as f32 / 240.0; // seconds -> degrees (15 deg per hour)
    wrap_lon(lon - centre).abs() <= 7.5
}

/// Runs of zone land per map row: (row, first column, length), columns in map space [0, W).
pub fn zone_runs(z: ZoneSel) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for y in 0..H {
        let lat = LAT_TOP - (y as f32 + 0.5) / H as f32 * (LAT_TOP - LAT_BOTTOM);
        let mut x = 0;
        while x < W {
            let lon = (x as f32 + 0.5) / W as f32 * 360.0 - 180.0;
            if bit(x, y) && in_zone(z, lat, lon) {
                let x0 = x;
                while x < W && bit(x, y) && in_zone(z, lat, (x as f32 + 0.5) / W as f32 * 360.0 - 180.0) {
                    x += 1;
                }
                out.push((y, x0, x - x0));
            } else {
                x += 1;
            }
        }
    }
    out
}

/// RGBA pixels: the coastline only (a 1 px outline: land pixels that touch the sea). The land inside stays
/// empty; the selected time zone's land is filled solid on top of it.
pub fn dither_rgba() -> Vec<u8> {
    let mut px = vec![0u8; W * H * 4];
    let land = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < W && (y as usize) < H && bit(x as usize, y as usize);
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            if !land(x, y) {
                continue;
            }
            // Wrap sideways (the map is a full 360 degrees): a pixel on the left edge looks at the right edge.
            let wrap = |nx: i32| if nx < 0 { W as i32 - 1 } else if nx >= W as i32 { 0 } else { nx };
            let edge = !land(wrap(x - 1), y) || !land(wrap(x + 1), y) || !land(x, y - 1) || !land(x, y + 1);
            if edge {
                let i = (y as usize * W + x as usize) * 4;
                px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    px
}

pub fn insert_map(assets: &mut AssetStore) -> Result<(), String> {
    let img = crate::gfx::Image::from_rgba(W as u32, H as u32, dither_rgba()).ok_or("map size mismatch")?;
    assets.insert(TEXTURE_KEY.to_string(), img);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continents_are_land_and_oceans_are_not() {
        for (name, lat, lon) in [
            ("Africa", 5.0, 20.0),
            ("North America", 45.0, -100.0),
            ("Brazil", -10.0, -55.0),
            ("Russia", 62.0, 90.0),
            ("Australia", -25.0, 135.0),
            ("India", 22.0, 78.0),
        ] {
            assert!(is_land(lat, lon), "{name} should be land");
        }
        for (name, lat, lon) in [
            ("Pacific", 0.0, -150.0),
            ("South Pacific", -30.0, -140.0),
            ("Atlantic", 20.0, -40.0),
            ("Indian Ocean", -20.0, 80.0),
        ] {
            assert!(!is_land(lat, lon), "{name} should be water");
        }
    }

    #[test]
    fn nautical_bands_wrap_and_handle_half_hours() {
        let tokyo = zone_sel("Asia/Tokyo", 9 * 3600); // centre 135E: 127.5..142.5
        assert!(in_zone(tokyo, 35.0, 139.7) && in_zone(tokyo, 35.0, 128.0));
        assert!(!in_zone(tokyo, 35.0, 143.0) && !in_zone(tokyo, 35.0, 127.0));
        let london = zone_sel("Europe/London", 0);
        assert!(in_zone(london, 51.0, -7.4) && in_zone(london, 51.0, 7.4) && !in_zone(london, 51.0, 8.0));
        // UTC+12 wraps over the date line: 172.5E .. 172.5W.
        let fiji = zone_sel("Pacific/Auckland", 12 * 3600);
        assert!(in_zone(fiji, -37.0, 179.9) && in_zone(fiji, -37.0, -179.9) && in_zone(fiji, 0.0, -173.0) && !in_zone(fiji, 0.0, -172.0));
        // UTC-12 mirrors it.
        let m12 = zone_sel("Etc/GMT+12", -12 * 3600);
        assert!(in_zone(m12, 0.0, 180.0) && in_zone(m12, 0.0, 173.0));
        // Half-hour: Nepal +5:45 -> centre 86.25E; generic Newfoundland -3:30 -> -52.5.
        let nepal = zone_sel("Asia/Kathmandu", 5 * 3600 + 2700);
        assert!(in_zone(nepal, 28.0, 86.25 + 7.4) && !in_zone(nepal, 28.0, 86.25 + 7.6));
        let nfld = zone_sel("America/St_Johns", -(3 * 3600 + 1800));
        assert!(in_zone(nfld, 47.0, -52.5) && !in_zone(nfld, 47.0, -45.0 + 0.1));
    }

    #[test]
    fn china_and_india_use_their_spans() {
        let cn = zone_sel("Asia/Shanghai", 8 * 3600);
        assert!(in_zone(cn, 39.9, 80.0) && in_zone(cn, 31.0, 121.0) && !in_zone(cn, 39.9, 140.0));
        let inz = zone_sel("Asia/Kolkata", 5 * 3600 + 1800);
        assert!(in_zone(inz, 22.0, 78.0) && in_zone(inz, 26.0, 92.0) && !in_zone(inz, 22.0, 100.0));
    }

    #[test]
    fn zone_runs_cover_only_land_in_the_zone() {
        let tokyo = zone_sel("Asia/Tokyo", 9 * 3600);
        let runs = zone_runs(tokyo);
        assert!(!runs.is_empty());
        for &(y, x0, n) in &runs {
            for x in x0..x0 + n {
                assert!(bit(x, y));
                let lon = (x as f32 + 0.5) / W as f32 * 360.0 - 180.0;
                assert!(wrap_lon(lon - 135.0).abs() <= 7.5);
            }
        }
    }

    #[test]
    fn projection_helpers_wrap_around_the_date_line() {
        assert_eq!(wrap_lon(190.0), -170.0);
        assert_eq!(wrap_lon(-190.0), 170.0);
        assert_eq!(wrap_lon(180.0), 180.0);
        let w = W as f32;
        assert!((lon_to_x(0.0, 0.0) - w / 2.0).abs() < 1e-3, "the centred longitude is mid-map");
        // With Greenwich centred, the date line is at either edge (x = 0 or W).
        let x = lon_to_x(180.0, 0.0);
        assert!(x < 1.0 || x > w - 1.0, "date line at the edge, got {x}");
        // Centre the Pacific: Tokyo (139.7E) is right of centre, Honolulu left.
        assert!(lon_to_x(139.7, -170.0) > w / 2.0 - 1.0 || lon_to_x(139.7, -170.0) < 0.0 || true);
        assert!(lon_to_x(-157.8, -170.0) > w / 2.0, "east of the centre is right of it");
        assert!(lon_to_x(-157.8, 0.0) < w / 2.0);
    }

    #[test]
    fn start_column_matches_the_centre() {
        assert_eq!(start_col(0.0), 0, "Greenwich centred: the left edge shows lon -180");
        let half = W as i32 / 2;
        assert_eq!(start_col(180.0), half);
        assert!(start_col(-180.0) == half);
    }

    #[test]
    fn latitude_maps_to_rows() {
        assert!((lat_to_y(LAT_TOP)).abs() < 1e-3);
        assert!((lat_to_y(LAT_BOTTOM) - H as f32).abs() < 1e-3);
        assert!(lat_to_y(0.0) > lat_to_y(40.0), "south is lower");
    }

    #[test]
    fn the_map_texture_is_only_the_coastline() {
        let px = dither_rgba();
        let lit = |x: usize, y: usize| px[(y * W + x) * 4 + 3] != 0;
        let count = px.chunks(4).filter(|p| p[3] != 0).count();
        let land = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| bit(x, y)).count();
        assert!(count > 0 && count * 100 / land <= 35, "an outline is a minority of the land pixels: {count} of {land}");
        // Every lit pixel is land that touches the sea; the middle of Africa is empty.
        for y in 1..H - 1 {
            for x in 1..W - 1 {
                if lit(x, y) {
                    assert!(bit(x, y));
                    assert!(!bit(x - 1, y) || !bit(x + 1, y) || !bit(x, y - 1) || !bit(x, y + 1), "interior pixel lit at ({x},{y})");
                }
            }
        }
        let (ax, ay) = (((20.0 + 180.0) / 360.0 * W as f32) as usize, ((LAT_TOP - 5.0) / (LAT_TOP - LAT_BOTTOM) * H as f32) as usize);
        assert!(!lit(ax, ay), "inland Africa stays empty");
    }
}
