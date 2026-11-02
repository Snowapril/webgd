use webgd_core::pixels::{Image, PixelFormat, f16_to_f64, gpu_format};

fn img(format: PixelFormat, w: u32, h: u32, data: Vec<u8>) -> Image {
    Image { width: w, height: h, format, data }
}

#[test]
fn f16_decode() {
    assert_eq!(f16_to_f64(0x3C00), 1.0);
    assert_eq!(f16_to_f64(0xC000), -2.0);
    assert_eq!(f16_to_f64(0x3555), 0.333251953125);
    assert_eq!(f16_to_f64(0x0001), 2f64.powi(-24));
    assert_eq!(f16_to_f64(0x7C00), f64::INFINITY);
    assert!(f16_to_f64(0x7E00).is_nan());
    // rgba16float texel (1, -2, 0.5, 65504)
    let data = [0x3C00u16, 0xC000, 0x3800, 0x7BFF].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(img(PixelFormat::Rgba16Float, 1, 1, data).texel(0, 0), [1.0, -2.0, 0.5, 65504.0]);
    let r16 = img(PixelFormat::R16Float, 1, 1, 0x4200u16.to_le_bytes().to_vec());
    assert_eq!(r16.texel(0, 0), [3.0, 0.0, 0.0, 1.0]);
}

#[test]
fn rg11b10_decode() {
    // R: exp 15 mant 0 = 1.0; G: exp 16 mant 32 = 3.0; B: exp 14 mant 16 = 0.75.
    let r = 15 << 6;
    let g = (16 << 6) | 32;
    let b = (14 << 5) | 16;
    let v: u32 = r | (g << 11) | (b << 22);
    let t = img(PixelFormat::Rg11b10Ufloat, 1, 1, v.to_le_bytes().to_vec()).texel(0, 0);
    assert_eq!(t, [1.0, 3.0, 0.75, 1.0]);
}

#[test]
fn rgb10a2_and_ints() {
    let v: u32 = 1023 | (341 << 20) | (3 << 30);
    let t = img(PixelFormat::Rgb10a2Unorm, 1, 1, v.to_le_bytes().to_vec()).texel(0, 0);
    assert_eq!(t, [1.0, 0.0, 341.0 / 1023.0, 1.0]);
    let s = img(PixelFormat::R32Sint, 1, 1, (-5i32).to_le_bytes().to_vec());
    assert_eq!(s.texel(0, 0), [-5.0, 0.0, 0.0, 1.0]);
    let bgra = img(PixelFormat::Bgra8Unorm, 1, 1, vec![1, 2, 3, 4]);
    assert_eq!(bgra.texel(0, 0), [3.0, 2.0, 1.0, 4.0]);
    assert_eq!(bgra.texel(5, 0), [0.0; 4]);
}

#[test]
fn srgb_passthrough() {
    let i = img(PixelFormat::Rgba8Srgb, 2, 1, vec![10, 20, 30, 40, 255, 0, 128, 255]);
    // Range is ignored for 8-bit formats.
    assert_eq!(i.to_display_rgba8((100.0, 200.0), [true; 4]), vec![10, 20, 30, 40, 255, 0, 128, 255]);
    let b = img(PixelFormat::Bgra8Srgb, 1, 1, vec![10, 20, 30, 40]);
    assert_eq!(b.to_display_rgba8((0.0, 1.0), [true; 4]), vec![30, 20, 10, 40]);
}

#[test]
fn range_mapping() {
    let data: Vec<u8> = [-1.0f32, 0.0, 1.0, 0.5, 3.0, 2.0, f32::NAN, 7.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let i = img(PixelFormat::Rgba32Float, 2, 1, data);
    // Range (0, 2): -1→0, 0→0, 1→128; alpha clamps 0..1: 0.5→128. 3→255, 2→255, NaN→0; alpha 7→255.
    assert_eq!(i.to_display_rgba8((0.0, 2.0), [true; 4]), vec![0, 0, 128, 128, 255, 255, 0, 255]);
    // auto range over RGB ignores NaN and alpha.
    assert_eq!(i.auto_range(), (-1.0, 3.0));
    let inf = img(PixelFormat::R32Float, 2, 1, [f32::INFINITY, 4.0].iter().flat_map(|v| v.to_le_bytes()).collect());
    assert_eq!(inf.auto_range(), (4.0, 5.0));
    // Single-channel format: missing G/B don't drag the range to 0.
    let r = img(PixelFormat::R32Float, 2, 1, [2.0f32, 6.0].iter().flat_map(|v| v.to_le_bytes()).collect());
    assert_eq!(r.auto_range(), (2.0, 6.0));
}

#[test]
fn channel_masking() {
    let i = img(PixelFormat::Rgba8Unorm, 1, 1, vec![10, 20, 30, 40]);
    assert_eq!(i.to_display_rgba8((0.0, 1.0), [true, false, true, false]), vec![10, 0, 30, 255]);
    // Single channel → grayscale, opaque.
    assert_eq!(i.to_display_rgba8((0.0, 1.0), [false, true, false, false]), vec![20, 20, 20, 255]);
    assert_eq!(i.to_display_rgba8((0.0, 1.0), [false, false, false, true]), vec![40, 40, 40, 255]);
    let f = img(PixelFormat::Rg32Float, 1, 1, [0.25f32, 0.75].iter().flat_map(|v| v.to_le_bytes()).collect());
    assert_eq!(f.to_display_rgba8((0.0, 1.0), [false, true, false, false]), vec![191, 191, 191, 255]);
    assert_eq!(f.to_display_rgba8((0.0, 1.0), [true, true, true, true]), vec![64, 191, 0, 255]);
}

#[test]
fn gpu_formats() {
    assert_eq!(gpu_format("rgba8unorm-srgb"), Some(PixelFormat::Rgba8Srgb));
    assert_eq!(gpu_format("bgra8unorm-srgb"), Some(PixelFormat::Bgra8Srgb));
    assert_eq!(gpu_format("rgba32uint"), Some(PixelFormat::Rgba32UintGpu));
    assert_eq!(gpu_format("depth32float"), Some(PixelFormat::Depth32Float));
    assert_eq!(gpu_format("stencil8"), Some(PixelFormat::Stencil8));
    assert_eq!(gpu_format("r32uint"), Some(PixelFormat::R32Uint));
    assert_eq!(gpu_format("bc1-rgba-unorm"), None);
    assert_eq!(gpu_format("depth24plus"), None);
    assert_eq!(PixelFormat::Rgba16Float.bytes_per_texel(), 8);
    assert_eq!(PixelFormat::Rg11b10Ufloat.bytes_per_texel(), 4);
}
