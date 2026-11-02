//! CPU-side pixel decoding for the texture viewer.
//!
//! `Image::texel` returns the stored value per channel:
//! - 8-bit formats (`Rgba8*`, `Bgra8*`, `R8Unorm`, `Rg8Unorm`): raw byte values 0..=255 in RGBA order.
//! - integer formats and `Stencil8`: the integer value.
//! - float formats: the float value; `Rg11b10Ufloat` decoded to f32.
//! - `Rgb10a2Unorm`, `Depth16Unorm`: normalized 0..=1.
//!
//! Missing channels are 0, missing alpha is 1 (255 for 8-bit formats).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    Rgba8Unorm,
    Rgba8Srgb,
    Bgra8Unorm,
    Bgra8Srgb,
    Rgba32Float,
    Rgba32Sint,
    /// GL `RGBA_INTEGER`/`UNSIGNED_INT` readback.
    Rgba32Uint,
    R8Unorm,
    Rg8Unorm,
    R16Float,
    Rg16Float,
    Rgba16Float,
    R32Float,
    Rg32Float,
    R32Uint,
    R32Sint,
    /// WebGPU `rgba32uint`.
    Rgba32UintGpu,
    Rg11b10Ufloat,
    Rgb10a2Unorm,
    Depth32Float,
    Depth16Unorm,
    Stencil8,
}

/// Tightly packed rows (no padding).
#[derive(Clone, PartialEq, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

impl PixelFormat {
    pub fn bytes_per_texel(self) -> u32 {
        use PixelFormat::*;
        match self {
            R8Unorm | Stencil8 => 1,
            Rg8Unorm | R16Float | Depth16Unorm => 2,
            Rgba8Unorm | Rgba8Srgb | Bgra8Unorm | Bgra8Srgb | Rg16Float | R32Float | R32Uint
            | R32Sint | Rg11b10Ufloat | Rgb10a2Unorm | Depth32Float => 4,
            Rgba16Float | Rg32Float => 8,
            Rgba32Float | Rgba32Sint | Rgba32Uint | Rgba32UintGpu => 16,
        }
    }

    /// Number of stored color channels (alpha included).
    pub fn channel_count(self) -> usize {
        use PixelFormat::*;
        match self {
            R8Unorm | R16Float | R32Float | R32Uint | R32Sint | Depth32Float | Depth16Unorm
            | Stencil8 => 1,
            Rg8Unorm | Rg16Float | Rg32Float => 2,
            Rg11b10Ufloat => 3,
            _ => 4,
        }
    }

    /// 8-bit formats whose texel values are shown as-is.
    pub fn is_passthrough(self) -> bool {
        use PixelFormat::*;
        matches!(
            self,
            Rgba8Unorm | Rgba8Srgb | Bgra8Unorm | Bgra8Srgb | R8Unorm | Rg8Unorm
        )
    }

    fn missing_alpha(self) -> f64 {
        if self.is_passthrough() { 255.0 } else { 1.0 }
    }
}

/// IEEE 754 binary16 → f64.
pub fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f64;
    sign * match exp {
        0 => mant / 1024.0 * 2f64.powi(-14),
        31 if mant == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + mant / 1024.0) * 2f64.powi(e - 15),
    }
}

/// Unsigned small float with a 5-bit exponent (bias 15) and `mbits` mantissa bits.
fn ufloat(bits: u32, mbits: u32) -> f64 {
    let mant = (bits & ((1 << mbits) - 1)) as f64;
    let exp = (bits >> mbits) as i32 & 0x1f;
    let scale = (1u32 << mbits) as f64;
    match exp {
        0 => mant / scale * 2f64.powi(-14),
        31 if mant == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + mant / scale) * 2f64.powi(e - 15),
    }
}

impl Image {
    pub fn texel(&self, x: u32, y: u32) -> [f64; 4] {
        use PixelFormat::*;
        let f = self.format;
        let bpt = f.bytes_per_texel() as usize;
        if x >= self.width || y >= self.height {
            return [0.0; 4];
        }
        let off = (y as usize * self.width as usize + x as usize) * bpt;
        let Some(p) = self.data.get(off..off + bpt) else {
            return [0.0; 4];
        };
        let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
        let f32_at = |i: usize| f32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]) as f64;
        let a1 = f.missing_alpha();
        let b = |i: usize| p[i] as f64;
        match f {
            Rgba8Unorm | Rgba8Srgb => [b(0), b(1), b(2), b(3)],
            Bgra8Unorm | Bgra8Srgb => [b(2), b(1), b(0), b(3)],
            R8Unorm => [b(0), 0.0, 0.0, a1],
            Rg8Unorm => [b(0), b(1), 0.0, a1],
            Stencil8 => [b(0), 0.0, 0.0, a1],
            R16Float => [f16_to_f64(u16_at(0)), 0.0, 0.0, a1],
            Rg16Float => [f16_to_f64(u16_at(0)), f16_to_f64(u16_at(2)), 0.0, a1],
            Rgba16Float => std::array::from_fn(|c| f16_to_f64(u16_at(c * 2))),
            Depth16Unorm => [u16_at(0) as f64 / 65535.0, 0.0, 0.0, a1],
            R32Float | Depth32Float => [f32_at(0), 0.0, 0.0, a1],
            Rg32Float => [f32_at(0), f32_at(4), 0.0, a1],
            Rgba32Float => std::array::from_fn(|c| f32_at(c * 4)),
            R32Uint => [u32_at(0) as f64, 0.0, 0.0, a1],
            R32Sint => [u32_at(0) as i32 as f64, 0.0, 0.0, a1],
            Rgba32Uint | Rgba32UintGpu => std::array::from_fn(|c| u32_at(c * 4) as f64),
            Rgba32Sint => std::array::from_fn(|c| u32_at(c * 4) as i32 as f64),
            Rg11b10Ufloat => {
                let v = u32_at(0);
                [
                    ufloat(v & 0x7ff, 6),
                    ufloat((v >> 11) & 0x7ff, 6),
                    ufloat(v >> 22, 5),
                    a1,
                ]
            }
            Rgb10a2Unorm => {
                let v = u32_at(0);
                [
                    (v & 0x3ff) as f64 / 1023.0,
                    ((v >> 10) & 0x3ff) as f64 / 1023.0,
                    ((v >> 20) & 0x3ff) as f64 / 1023.0,
                    (v >> 30) as f64 / 3.0,
                ]
            }
        }
    }

    /// RGBA8 for display. 8-bit formats pass through; other formats map `range` linearly
    /// onto 0..=255 (alpha of float/normalized formats is clamped 0..=1 instead). Disabled
    /// channels are 0 (disabled alpha is 255). A single enabled channel is shown as grayscale.
    pub fn to_display_rgba8(&self, range: (f64, f64), channels: [bool; 4]) -> Vec<u8> {
        use PixelFormat::*;
        let f = self.format;
        let pass = f.is_passthrough();
        let int_alpha = matches!(f, Rgba32Uint | Rgba32UintGpu | Rgba32Sint);
        let (lo, hi) = range;
        let span = hi - lo;
        let map = |v: f64| -> u8 {
            if pass {
                return v.clamp(0.0, 255.0) as u8;
            }
            let t = if span > 0.0 {
                (v - lo) / span
            } else if v >= lo {
                1.0
            } else {
                0.0
            };
            if t.is_nan() {
                0
            } else {
                (t.clamp(0.0, 1.0) * 255.0).round() as u8
            }
        };
        let map_alpha = |v: f64| -> u8 {
            if pass || int_alpha {
                map(v)
            } else {
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            }
        };
        let single = (channels.iter().filter(|&&c| c).count() == 1)
            .then(|| channels.iter().position(|&c| c).unwrap());
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height {
            for x in 0..self.width {
                let t = self.texel(x, y);
                if let Some(c) = single {
                    let g = if c == 3 { map_alpha(t[3]) } else { map(t[c]) };
                    out.extend_from_slice(&[g, g, g, 255]);
                } else {
                    for c in 0..3 {
                        out.push(if channels[c] { map(t[c]) } else { 0 });
                    }
                    out.push(if channels[3] { map_alpha(t[3]) } else { 255 });
                }
            }
        }
        out
    }

    /// Min/max over the stored RGB channels, ignoring NaN/Inf. `(0, 1)` if nothing finite;
    /// a degenerate range is widened to `(v, v + 1)`.
    pub fn auto_range(&self) -> (f64, f64) {
        let n = self.format.channel_count().min(3);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for y in 0..self.height {
            for x in 0..self.width {
                for &v in &self.texel(x, y)[..n] {
                    if v.is_finite() {
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
            }
        }
        if lo > hi {
            (0.0, 1.0)
        } else if lo == hi {
            (lo, lo + 1.0)
        } else {
            (lo, hi)
        }
    }
}

/// WebGPU texture format string → viewable pixel format.
pub fn gpu_format(format: &str) -> Option<PixelFormat> {
    use PixelFormat::*;
    Some(match format {
        "rgba8unorm" => Rgba8Unorm,
        "rgba8unorm-srgb" => Rgba8Srgb,
        "bgra8unorm" => Bgra8Unorm,
        "bgra8unorm-srgb" => Bgra8Srgb,
        "rgba32float" => Rgba32Float,
        "rgba32sint" => Rgba32Sint,
        "rgba32uint" => Rgba32UintGpu,
        "r8unorm" => R8Unorm,
        "rg8unorm" => Rg8Unorm,
        "r16float" => R16Float,
        "rg16float" => Rg16Float,
        "rgba16float" => Rgba16Float,
        "r32float" => R32Float,
        "rg32float" => Rg32Float,
        "r32uint" => R32Uint,
        "r32sint" => R32Sint,
        "rg11b10ufloat" => Rg11b10Ufloat,
        "rgb10a2unorm" => Rgb10a2Unorm,
        "depth32float" => Depth32Float,
        "depth16unorm" => Depth16Unorm,
        "stencil8" => Stencil8,
        _ => return None,
    })
}
