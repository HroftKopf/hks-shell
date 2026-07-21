//! Application icons: resolve a freedesktop icon name to a file, decode it
//! (SVG via resvg, raster via image), and rasterize on demand for glyphon's
//! custom-glyph mechanism (so icons ride the existing text pipeline).

use std::path::{Path, PathBuf};

use glyphon::{ContentType, RasterizeCustomGlyphRequest, RasterizedCustomGlyph};

/// A decoded icon source, rasterized on demand at the requested size.
pub enum IconSource {
    Svg(resvg::usvg::Tree),
    Raster(image::RgbaImage),
}

/// Resolve an icon name (or absolute path) from the `.desktop` `Icon=` key to a
/// decoded source. Returns `None` if it can't be found or decoded.
pub fn resolve_icon(name: &str) -> Option<IconSource> {
    let path: PathBuf = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        freedesktop_icons::lookup(name)
            .with_size(64)
            .with_cache()
            .find()?
    };

    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "svg" | "svgz" => {
            let data = std::fs::read(&path).ok()?;
            let tree =
                resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
            Some(IconSource::Svg(tree))
        }
        _ => {
            // PNG (and any other raster format enabled on the `image` crate).
            let img = image::open(&path).ok()?.to_rgba8();
            Some(IconSource::Raster(img))
        }
    }
}

/// Rasterize an icon (by id = index into `sources`) at the size glyphon asks
/// for, returning premultiplied RGBA (glyphon `ContentType::Color`).
pub fn rasterize_icon(
    sources: &[IconSource],
    req: RasterizeCustomGlyphRequest,
) -> Option<RasterizedCustomGlyph> {
    let source = sources.get(req.id as usize)?;
    let (w, h) = (req.width as u32, req.height as u32);
    if w == 0 || h == 0 {
        return None;
    }

    let data = match source {
        IconSource::Svg(tree) => {
            let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
            let size = tree.size();
            let scale = (w as f32 / size.width()).min(h as f32 / size.height());
            let tx = (w as f32 - size.width() * scale) * 0.5;
            let ty = (h as f32 - size.height() * scale) * 0.5;
            let transform =
                resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
            resvg::render(tree, transform, &mut pixmap.as_mut());
            pixmap.data().to_vec()
        }
        IconSource::Raster(img) => {
            let resized =
                image::imageops::resize(img, w, h, image::imageops::FilterType::Lanczos3);
            let mut data = resized.into_raw();
            // Premultiply alpha to match glyphon's color-glyph expectation.
            for px in data.chunks_exact_mut(4) {
                let a = px[3] as u16;
                px[0] = (px[0] as u16 * a / 255) as u8;
                px[1] = (px[1] as u16 * a / 255) as u8;
                px[2] = (px[2] as u16 * a / 255) as u8;
            }
            data
        }
    };

    Some(RasterizedCustomGlyph {
        data,
        content_type: ContentType::Color,
    })
}
