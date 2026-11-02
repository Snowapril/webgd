//! WebGL2 internal-format tables (mirrors hook.js `FORMAT_CLASS`).
use super::c::*;
use webgd_core::pixels::PixelFormat;

/// How a color format is read back with `readPixels` (and restored with a temp texture).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadClass {
    Unorm,
    Srgb,
    Float,
    Int,
    Uint,
}

impl ReadClass {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unorm" => ReadClass::Unorm,
            "srgb" => ReadClass::Srgb,
            "float" => ReadClass::Float,
            "int" => ReadClass::Int,
            "uint" => ReadClass::Uint,
            _ => return None,
        })
    }

    /// (readPixels format, readPixels type, typed array ctor, image format).
    pub fn read_spec(self) -> (u32, u32, &'static str, PixelFormat) {
        match self {
            ReadClass::Unorm => (RGBA, UNSIGNED_BYTE, "Uint8Array", PixelFormat::Rgba8Unorm),
            ReadClass::Srgb => (RGBA, UNSIGNED_BYTE, "Uint8Array", PixelFormat::Rgba8Srgb),
            ReadClass::Float => (RGBA, FLOAT, "Float32Array", PixelFormat::Rgba32Float),
            ReadClass::Int => (RGBA_INTEGER, INT, "Int32Array", PixelFormat::Rgba32Sint),
            ReadClass::Uint => (
                RGBA_INTEGER,
                UNSIGNED_INT,
                "Uint32Array",
                PixelFormat::Rgba32Uint,
            ),
        }
    }

    /// Internal format of the temp texture used to blit restored contents.
    pub fn temp_internalformat(self) -> u32 {
        match self {
            ReadClass::Unorm => RGBA8,
            ReadClass::Srgb => SRGB8_ALPHA8,
            ReadClass::Float => RGBA32F,
            ReadClass::Int => RGBA32I,
            ReadClass::Uint => RGBA32UI,
        }
    }
}

pub fn is_depth_stencil(internalformat: u32) -> bool {
    matches!(
        internalformat,
        DEPTH_COMPONENT16
            | DEPTH_COMPONENT24
            | DEPTH_COMPONENT32F
            | DEPTH24_STENCIL8
            | DEPTH32F_STENCIL8
            | STENCIL_INDEX8
            | DEPTH_COMPONENT
            | DEPTH_STENCIL
    )
}

/// Read class of a color internal format; `None` for depth/stencil.
pub fn read_class(internalformat: u32) -> Option<ReadClass> {
    if is_depth_stencil(internalformat) {
        return None;
    }
    let (format, ty) = canonical_format_type(internalformat).unwrap_or((RGBA, UNSIGNED_BYTE));
    Some(match internalformat {
        SRGB8_ALPHA8 | SRGB8 => ReadClass::Srgb,
        _ if matches!(
            format,
            RED_INTEGER | RG_INTEGER | RGB_INTEGER | RGBA_INTEGER
        ) =>
        {
            match ty {
                BYTE | SHORT | INT => ReadClass::Int,
                _ => ReadClass::Uint,
            }
        }
        _ if matches!(ty, FLOAT | HALF_FLOAT) => ReadClass::Float,
        _ => ReadClass::Unorm,
    })
}

/// (format, type) valid for `texImage*(…, internalformat, …, format, type, null)`.
pub fn canonical_format_type(internalformat: u32) -> Option<(u32, u32)> {
    Some(match internalformat {
        RGBA | RGB | LUMINANCE | ALPHA | LUMINANCE_ALPHA => (internalformat, UNSIGNED_BYTE),
        R8 => (RED, UNSIGNED_BYTE),
        R8_SNORM => (RED, BYTE),
        R16F => (RED, HALF_FLOAT),
        R32F => (RED, FLOAT),
        R8UI => (RED_INTEGER, UNSIGNED_BYTE),
        R8I => (RED_INTEGER, BYTE),
        R16UI => (RED_INTEGER, UNSIGNED_SHORT),
        R16I => (RED_INTEGER, SHORT),
        R32UI => (RED_INTEGER, UNSIGNED_INT),
        R32I => (RED_INTEGER, INT),
        RG8 => (RG, UNSIGNED_BYTE),
        RG8_SNORM => (RG, BYTE),
        RG16F => (RG, HALF_FLOAT),
        RG32F => (RG, FLOAT),
        RG8UI => (RG_INTEGER, UNSIGNED_BYTE),
        RG8I => (RG_INTEGER, BYTE),
        RG16UI => (RG_INTEGER, UNSIGNED_SHORT),
        RG16I => (RG_INTEGER, SHORT),
        RG32UI => (RG_INTEGER, UNSIGNED_INT),
        RG32I => (RG_INTEGER, INT),
        RGB8 | SRGB8 | RGB565 => (RGB, UNSIGNED_BYTE),
        RGB8_SNORM => (RGB, BYTE),
        R11F_G11F_B10F | RGB16F => (RGB, HALF_FLOAT),
        RGB9_E5 | RGB32F => (RGB, FLOAT),
        RGB8UI => (RGB_INTEGER, UNSIGNED_BYTE),
        RGB8I => (RGB_INTEGER, BYTE),
        RGB16UI => (RGB_INTEGER, UNSIGNED_SHORT),
        RGB16I => (RGB_INTEGER, SHORT),
        RGB32UI => (RGB_INTEGER, UNSIGNED_INT),
        RGB32I => (RGB_INTEGER, INT),
        RGBA8 | SRGB8_ALPHA8 | RGB5_A1 | RGBA4 => (RGBA, UNSIGNED_BYTE),
        RGBA8_SNORM => (RGBA, BYTE),
        RGB10_A2 => (RGBA, UNSIGNED_INT_2_10_10_10_REV),
        RGBA16F => (RGBA, HALF_FLOAT),
        RGBA32F => (RGBA, FLOAT),
        RGBA8UI => (RGBA_INTEGER, UNSIGNED_BYTE),
        RGBA8I => (RGBA_INTEGER, BYTE),
        RGB10_A2UI => (RGBA_INTEGER, UNSIGNED_INT_2_10_10_10_REV),
        RGBA16UI => (RGBA_INTEGER, UNSIGNED_SHORT),
        RGBA16I => (RGBA_INTEGER, SHORT),
        RGBA32UI => (RGBA_INTEGER, UNSIGNED_INT),
        RGBA32I => (RGBA_INTEGER, INT),
        DEPTH_COMPONENT16 | DEPTH_COMPONENT24 => (DEPTH_COMPONENT, UNSIGNED_INT),
        DEPTH_COMPONENT32F => (DEPTH_COMPONENT, FLOAT),
        DEPTH24_STENCIL8 => (DEPTH_STENCIL, UNSIGNED_INT_24_8),
        DEPTH32F_STENCIL8 => (DEPTH_STENCIL, FLOAT_32_UNSIGNED_INT_24_8_REV),
        _ => return None,
    })
}
