//! CPU-side pixel decoding for the texture viewer.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    Rgba8Unorm,
    Rgba8Srgb,
    Bgra8Unorm,
    Bgra8Srgb,
    Rgba32Float,
    Rgba32Sint,
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
        todo!("core agent")
    }
}

impl Image {
    pub fn texel(&self, x: u32, y: u32) -> [f64; 4] {
        let _ = (x, y);
        todo!("core agent")
    }
    pub fn to_display_rgba8(&self, range: (f64, f64), channels: [bool; 4]) -> Vec<u8> {
        let _ = (range, channels);
        todo!("core agent")
    }
    pub fn auto_range(&self) -> (f64, f64) {
        todo!("core agent")
    }
}

/// WebGPU texture format string → viewable pixel format.
pub fn gpu_format(format: &str) -> Option<PixelFormat> {
    let _ = format;
    todo!("core agent")
}
