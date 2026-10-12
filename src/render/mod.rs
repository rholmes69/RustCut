//! Layout, text and colour maths shared by the live preview and the exporter, so
//! what you see is what you export.

pub mod filters;
pub mod text;

/// Size of a layer in canvas pixels: the media is fitted inside the canvas
/// ("contain") and then multiplied by `scale`.
pub fn layer_size(media_w: f64, media_h: f64, canvas_w: f64, canvas_h: f64, scale: f64) -> (f64, f64) {
    if media_w <= 0.0 || media_h <= 0.0 {
        return (canvas_w * scale, canvas_h * scale);
    }
    let fit = (canvas_w / media_w).min(canvas_h / media_h);
    (media_w * fit * scale, media_h * fit * scale)
}

/// Scale value that makes a layer cover the whole canvas ("fill").
pub fn fill_scale(media_w: f64, media_h: f64, canvas_w: f64, canvas_h: f64) -> f64 {
    if media_w <= 0.0 || media_h <= 0.0 {
        return 1.0;
    }
    let fit = (canvas_w / media_w).min(canvas_h / media_h);
    let fill = (canvas_w / media_w).max(canvas_h / media_h);
    fill / fit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_and_fill() {
        // Portrait clip in a landscape canvas: height-limited.
        assert_eq!(layer_size(1080.0, 1920.0, 1920.0, 1080.0, 1.0), (607.5, 1080.0));
        let s = fill_scale(1080.0, 1920.0, 1920.0, 1080.0);
        let (w, _) = layer_size(1080.0, 1920.0, 1920.0, 1080.0, s);
        assert!((w - 1920.0).abs() < 1e-6);
    }
}
