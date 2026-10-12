//! Colour filters. Each preset is defined once as a colour matrix and/or an
//! "eq" operation; [`apply_cpu`] runs it on preview frames and [`ffmpeg_filters`]
//! turns the very same numbers into FFmpeg filters for export.

use crate::model::{Adjust, FilterPreset};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqOp {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

type Matrix = [[f32; 3]; 3];

const SEPIA: Matrix = [[0.393, 0.769, 0.189], [0.349, 0.686, 0.168], [0.272, 0.534, 0.131]];

const fn diag(r: f32, g: f32, b: f32) -> Matrix {
    [[r, 0.0, 0.0], [0.0, g, 0.0], [0.0, 0.0, b]]
}

fn preset_ops(p: FilterPreset) -> (Option<Matrix>, Option<EqOp>) {
    match p {
        FilterPreset::None => (None, None),
        FilterPreset::BlackWhite => (None, Some(EqOp { brightness: 0.0, contrast: 1.05, saturation: 0.0 })),
        FilterPreset::Vintage => (Some(SEPIA), Some(EqOp { brightness: 0.0, contrast: 0.95, saturation: 1.0 })),
        FilterPreset::Warm => (Some(diag(1.10, 1.0, 0.88)), None),
        FilterPreset::Cool => (Some(diag(0.90, 1.0, 1.10)), None),
        FilterPreset::Vivid => (None, Some(EqOp { brightness: 0.0, contrast: 1.08, saturation: 1.45 })),
        FilterPreset::Matte => (None, Some(EqOp { brightness: 0.04, contrast: 0.85, saturation: 0.85 })),
    }
}

fn adjust_op(a: &Adjust) -> Option<EqOp> {
    (!a.is_identity()).then_some(EqOp { brightness: a.brightness, contrast: a.contrast, saturation: a.saturation })
}

pub fn is_identity(p: FilterPreset, a: &Adjust) -> bool {
    p == FilterPreset::None && a.is_identity()
}

fn eq(c: [f32; 3], e: EqOp) -> [f32; 3] {
    let o = c.map(|v| (v - 0.5) * e.contrast + 0.5 + e.brightness);
    let l = 0.299 * o[0] + 0.587 * o[1] + 0.114 * o[2];
    o.map(|v| l + (v - l) * e.saturation)
}

fn mul(m: &Matrix, c: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
        m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
        m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
    ]
}

/// Applies the preset then the manual adjustments to an RGBA buffer in place.
pub fn apply_cpu(rgba: &mut [u8], preset: FilterPreset, adjust: &Adjust) {
    let (matrix, preset_eq) = preset_ops(preset);
    let adj = adjust_op(adjust);
    if matrix.is_none() && preset_eq.is_none() && adj.is_none() {
        return;
    }
    for px in rgba.chunks_exact_mut(4) {
        let mut c = [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0];
        if let Some(m) = &matrix {
            c = mul(m, c);
        }
        if let Some(e) = preset_eq {
            c = eq(c, e);
        }
        if let Some(e) = adj {
            c = eq(c, e);
        }
        for i in 0..3 {
            px[i] = (c[i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
}

/// The equivalent FFmpeg filter chain (in order).
pub fn ffmpeg_filters(preset: FilterPreset, adjust: &Adjust) -> Vec<String> {
    let (matrix, preset_eq) = preset_ops(preset);
    let mut out = Vec::new();
    if let Some(m) = matrix {
        out.push(format!(
            "colorchannelmixer=rr={:.3}:rg={:.3}:rb={:.3}:gr={:.3}:gg={:.3}:gb={:.3}:br={:.3}:bg={:.3}:bb={:.3}",
            m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]
        ));
    }
    for e in [preset_eq, adjust_op(adjust)].into_iter().flatten() {
        out.push(format!(
            "eq=brightness={:.3}:contrast={:.3}:saturation={:.3}",
            e.brightness, e.contrast, e.saturation
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_noop() {
        let mut px = vec![10, 120, 250, 255];
        apply_cpu(&mut px, FilterPreset::None, &Adjust::default());
        assert_eq!(px, vec![10, 120, 250, 255]);
        assert!(ffmpeg_filters(FilterPreset::None, &Adjust::default()).is_empty());
    }

    #[test]
    fn black_and_white_is_grey() {
        let mut px = vec![200, 30, 90, 255];
        apply_cpu(&mut px, FilterPreset::BlackWhite, &Adjust::default());
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn ffmpeg_chain_order() {
        let adj = Adjust { brightness: 0.1, contrast: 1.0, saturation: 1.0 };
        let f = ffmpeg_filters(FilterPreset::Warm, &adj);
        assert_eq!(f.len(), 2);
        assert!(f[0].starts_with("colorchannelmixer=rr=1.100"));
        assert!(f[1].starts_with("eq=brightness=0.100"));
    }
}
