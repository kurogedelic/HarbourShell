//! AssetStore - PNG アセットの一括ロードと、テーマ色による染色描画。
//!
//! 設計:
//! - 全 PNG は「白×透明」で描かれている（tools/gen_widgets.py が出力）。
//! - `Canvas::blit` の染色（tint）で白ピクセルを tone 色に置換して描画する。
//!   ※ 染色は乗算なので、白(255)以外は暗くなる。黒基準 PNG は
//!      色が乗らないため、アセットは必ず白基準でなければならない。
//! - Classic ベベルは「複数 PNG を重ね描き」で表現する（blit_layers）。
//!
//! PNG のデコードは `crate::imgload` だけが行い、ここでは `gfx::Image` を持つ。

use std::collections::HashMap;

use crate::gfx::{Canvas, Color, Image, Rect};

/// アセットルート（実行時のカレントディレクトリ基準の相対パス）。
const ASSET_ROOT: &str = "assets/images";

/// PNG テクスチャを名前（相対パスから拡張子を除いたもの）で引けるストア。
pub struct AssetStore {
    textures: HashMap<String, Image>,
    /// Icon PNGs (`icons/*`) split into [ink, muted, white] white-mask layers; see `split_layers`.
    layers: HashMap<String, [Option<Image>; 3]>,
}

/// Colours an icon is drawn in: black -> `ink`, grey -> `muted`, white -> `white`.
/// `None` omits that layer (the plate shows through).
#[derive(Clone, Copy, Debug)]
pub struct IconPalette {
    pub ink: Color,
    pub muted: Option<Color>,
    pub white: Option<Color>,
}

/// Classify every pixel of a 32-bit RGBA image: transparent (alpha < 128) is nothing,
/// luminance < 64 is black, > 192 is white, the rest is grey. Returns three white-mask RGBA
/// images [black, grey, white] (alpha 255 where the pixel belongs to the layer).
/// An image with only white pixels is a legacy 1-bit icon (white mask tinted with the ink):
/// its white pixels become the black layer, so old icons keep their look.
pub fn split_layers(rgba: &[u8]) -> [Vec<u8>; 3] {
    let n = rgba.len() / 4;
    let mut cls = vec![3u8; n]; // 0 black, 1 grey, 2 white, 3 none
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        if px[3] < 128 {
            continue;
        }
        let l = (px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000;
        cls[i] = if l < 64 { 0 } else if l > 192 { 2 } else { 1 };
    }
    let legacy = !cls.iter().any(|&c| c == 0 || c == 1) && cls.iter().any(|&c| c == 2);
    let mut out = [vec![0u8; n * 4], vec![0u8; n * 4], vec![0u8; n * 4]];
    for (i, &c) in cls.iter().enumerate() {
        let layer = match c {
            3 => continue,
            2 if legacy => 0,
            c => c as usize,
        };
        out[layer][i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    out
}

impl AssetStore {
    /// 指定ディレクトリ以下の PNG を再帰的にロードする。
    /// 失敗したファイルは警告を出してスキップ（続行）。
    pub fn load_dir(dir: &str) -> Result<Self, String> {
        let mut textures: HashMap<String, Image> = HashMap::new();
        let mut layers: HashMap<String, [Option<Image>; 3]> = HashMap::new();

        let root = std::path::Path::new(dir);
        if !root.exists() {
            eprintln!("[assets] directory not found: {}", dir);
            return Ok(AssetStore { textures, layers: HashMap::new() });
        }

        let entries = collect_pngs(root);
        for path in entries {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with("icons/") {
                match load_layers(&path) {
                    Ok(l) => {
                        layers.insert(rel.clone(), l);
                    }
                    Err(e) => eprintln!("[assets] icon layers {}: {}", path.display(), e),
                }
            }
            match load_texture(&path) {
                Ok(tex) => {
                    textures.insert(rel, tex);
                }
                Err(e) => {
                    eprintln!("[assets] failed to load {}: {}", path.display(), e);
                }
            }
        }

        println!("[assets] loaded {} textures from {}", textures.len(), dir);
        Ok(AssetStore { textures, layers })
    }

    /// `assets/images` を既定のルートとしてロードする convenience。
    pub fn new() -> Result<Self, String> {
        Self::load_dir(ASSET_ROOT)
    }

    /// Load one icon PNG under `key`, as a texture and as black / grey / white layers (like `icons/*`).
    pub fn load_icon_file(&mut self, key: &str, path: &std::path::Path) -> Result<(), String> {
        let layers = load_layers(path)?;
        let tex = load_texture(path)?;
        self.layers.insert(key.to_string(), layers);
        self.textures.insert(key.to_string(), tex);
        Ok(())
    }

    /// Load every PNG under `dir` as `<prefix>/<relative path without .png>`, each also in layers
    /// (an app's `res/` folder). Unreadable files are skipped with a warning.
    pub fn load_tree(&mut self, dir: &std::path::Path, prefix: &str) {
        for path in collect_pngs(dir) {
            let rel = path.strip_prefix(dir).unwrap_or(&path).with_extension("").to_string_lossy().replace('\\', "/");
            if let Err(e) = self.load_icon_file(&format!("{prefix}/{rel}"), &path) {
                eprintln!("[assets] {}: {e}", path.display());
            }
        }
    }

    /// キーからテクスチャを取得。存在しなければ None。
    pub fn get(&self, key: &str) -> Option<&Image> {
        self.textures.get(key)
    }

    /// Register a runtime-made icon in the three-tone layer form (black / grey / white), so `blit_icon` can
    /// draw it in the theme's ink, muted and icon-white colours.
    pub fn insert_icon_layers(&mut self, key: &str, rgba: &[u8], w: usize, h: usize) -> Result<(), String> {
        let l = layers_from_rgba(rgba, w, h)?;
        self.layers.insert(key.to_string(), l);
        Ok(())
    }

    /// 実行時に生成したテクスチャ（変換済みアイコン等）を登録する。
    pub fn insert(&mut self, key: String, image: Image) {
        self.textures.insert(key, image);
    }

    /// ロード済みテクスチャ数。
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// テクスチャのサイズを取得。
    pub fn size(&self, key: &str) -> Option<(u32, u32)> {
        self.textures.get(key).map(|t| (t.width(), t.height()))
    }

    /// 単色染色して描画。
    /// - `key`: アセットキー（例: "statusbar/anchor"）
    /// - `dst`: 描画先矩形（None ならテクスチャ本来のサイズで左上原点）
    /// - `tone`: 染色する色
    pub fn blit_tinted(
        &mut self,
        canvas: &mut Canvas,
        key: &str,
        dst: Option<Rect>,
        tone: Color,
    ) -> Result<(), String> {
        let tex = self.textures.get(key).ok_or_else(|| format!("asset not found: {}", key))?;
        let dst_rect = match dst {
            Some(r) => r,
            None => Rect::new(0, 0, tex.width(), tex.height()),
        };
        canvas.blit(tex, None, dst_rect, tone)
    }

    /// テクスチャの一部（src）を染色して dst へ描く（左右にループするスクロール用）。
    pub fn blit_tinted_src(
        &mut self,
        canvas: &mut Canvas,
        key: &str,
        src: Rect,
        dst: Rect,
        tone: Color,
    ) -> Result<(), String> {
        let tex = self.textures.get(key).ok_or_else(|| format!("asset not found: {}", key))?;
        canvas.blit(tex, src, dst, tone)
    }

    /// Draw an icon in the three-tone palette. Icons without layers (generated `tile/<id>`
    /// masks) are drawn in `palette.ink` alone.
    pub fn blit_icon(&mut self, canvas: &mut Canvas, key: &str, dst: Rect, palette: IconPalette) -> Result<(), String> {
        let Some(l) = self.layers.get(key) else {
            return self.blit_tinted(canvas, key, Some(dst), palette.ink);
        };
        let tones = [Some(palette.ink), palette.muted, palette.white];
        for (tex, tone) in l.iter().zip(tones) {
            if let (Some(tex), Some(t)) = (tex.as_ref(), tone) {
                canvas.blit(tex, None, dst, t)?;
            }
        }
        Ok(())
    }

    /// 複数のレイヤー PNG を同位置に重ね描き（Classic ベベル用）。
    /// - `keys`: 下から順に描画するアセットキー
    /// - `tones`: 各レイヤーの染色色（keys と同長）
    /// - `dst`: 描画先矩形（全レイヤー共通）
    pub fn blit_layers(
        &mut self,
        canvas: &mut Canvas,
        keys: &[&str],
        tones: &[Color],
        dst: Option<Rect>,
    ) -> Result<(), String> {
        if keys.len() != tones.len() {
            return Err("keys and tones length mismatch".to_string());
        }
        if keys.is_empty() {
            return Ok(());
        }
        // 最初のレイヤーで dst の実サイズを確定
        let dst_rect = match dst {
            Some(r) => r,
            None => {
                let first = self
                    .textures
                    .get(keys[0])
                    .ok_or_else(|| format!("asset not found: {}", keys[0]))?;
                Rect::new(0, 0, first.width(), first.height())
            }
        };
        for (k, tone) in keys.iter().zip(tones.iter()) {
            let tex = self.textures.get(*k).ok_or_else(|| format!("asset not found: {}", k))?;
            canvas.blit(tex, None, dst_rect, *tone)?;
        }
        Ok(())
    }
}

/// Load an icon PNG and build its [black, grey, white] layer images (empty layers are None).
fn load_layers(path: &std::path::Path) -> Result<[Option<Image>; 3], String> {
    let (rgba, w, h) = crate::imgload::load_rgba(path)?;
    layers_from_rgba(&rgba, w, h)
}

/// Build the [black, grey, white] layer images of an RGBA image (empty layers are None).
fn layers_from_rgba(rgba: &[u8], w: usize, h: usize) -> Result<[Option<Image>; 3], String> {
    let mut out: [Option<Image>; 3] = [None, None, None];
    for (i, layer) in split_layers(rgba).into_iter().enumerate() {
        if !layer.chunks_exact(4).any(|p| p[3] != 0) {
            continue;
        }
        out[i] = Some(Image::from_rgba(w as u32, h as u32, layer).ok_or("layer size mismatch")?);
    }
    Ok(out)
}

/// PNG をロードして Image にする。
fn load_texture(path: &std::path::Path) -> Result<Image, String> {
    crate::imgload::load_image(path)
}

/// 指定ディレクトリ以下の .png を再帰的に集める（ソート済み）。
fn collect_pngs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    collect_pngs_inner(dir, &mut out);
    out.sort();
    out
}

fn collect_pngs_inner(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // macOS AppleDouble sidecars (`._name.png`) arrive with tar archives: not images.
        if path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("._")).unwrap_or(false) {
            continue;
        }
        if path.is_dir() {
            // 隠しディレクトリ（.DS_Store 等）は中身を見ない
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
            {
                continue;
            }
            collect_pngs_inner(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("png") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(px: &[[u8; 4]]) -> Vec<u8> {
        px.iter().flatten().copied().collect()
    }
    fn on(l: &[u8], i: usize) -> bool {
        l[i * 4 + 3] != 0
    }

    #[test]
    fn splits_black_grey_white_and_transparent() {
        let l = split_layers(&img(&[[0, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255], [0, 0, 0, 0], [255, 255, 255, 100]]));
        assert!(on(&l[0], 0) && !on(&l[1], 0) && !on(&l[2], 0));
        assert!(on(&l[1], 1) && !on(&l[0], 1));
        assert!(on(&l[2], 2) && !on(&l[0], 2));
        for k in 0..3 {
            assert!(!on(&l[k], 3) && !on(&l[k], 4), "alpha < 128 is nothing");
        }
    }

    #[test]
    fn thresholds() {
        let l = split_layers(&img(&[[63, 63, 63, 255], [64, 64, 64, 255], [192, 192, 192, 255], [193, 193, 193, 255]]));
        assert!(on(&l[0], 0) && on(&l[1], 1) && on(&l[1], 2) && on(&l[2], 3));
    }

    #[test]
    fn white_only_icons_are_legacy_ink() {
        let l = split_layers(&img(&[[255, 255, 255, 255], [0, 0, 0, 0]]));
        assert!(on(&l[0], 0) && !on(&l[2], 0), "old white masks draw in the ink");
    }

    #[test]
    fn mixed_icons_keep_white_as_white() {
        let l = split_layers(&img(&[[255, 255, 255, 255], [0, 0, 0, 255]]));
        assert!(on(&l[2], 0) && on(&l[0], 1));
    }
}
