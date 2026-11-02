//! WebGPU replay helpers: usage augmentation, readback.
pub mod inspect;
pub mod snapshot;

use crate::engine::{await_value, jserr};
use crate::js;
use wasm_bindgen::JsValue;
use webgd_core::pixels::{Image, PixelFormat, gpu_format};

pub mod buf_usage {
    pub const MAP_READ: u32 = 0x1;
    pub const MAP_WRITE: u32 = 0x2;
    pub const COPY_SRC: u32 = 0x4;
    pub const COPY_DST: u32 = 0x8;
    pub const QUERY_RESOLVE: u32 = 0x200;
}

pub mod tex_usage {
    pub const COPY_SRC: u32 = 0x1;
    pub const COPY_DST: u32 = 0x2;
    pub const TEXTURE_BINDING: u32 = 0x4;
    pub const RENDER_ATTACHMENT: u32 = 0x10;
    pub const TRANSIENT_ATTACHMENT: u32 = 0x20;
}

/// Hook augmentation (COPY_SRC unless MAP_READ) plus COPY_DST for snapshot writes.
pub fn augment_buffer_usage(usage: u32) -> u32 {
    use buf_usage::*;
    let mut u = usage;
    if u & MAP_READ == 0 {
        u |= COPY_SRC;
    }
    if u & (MAP_READ | MAP_WRITE) == 0 {
        u |= COPY_DST;
    }
    u
}

pub fn augment_texture_usage(usage: u32, sample_count: u32) -> u32 {
    use tex_usage::*;
    if sample_count <= 1 && usage & TRANSIENT_ATTACHMENT == 0 { usage | COPY_SRC | COPY_DST } else { usage }
}

fn align(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

/// Maps a MAP_READ staging buffer and copies `len` bytes out of it.
pub(crate) async fn map_read(staging: &JsValue, len: u32) -> Result<Vec<u8>, String> {
    let p = js::call(staging, "mapAsync", &[js::num(buf_usage::MAP_READ as f64)]).map_err(jserr)?;
    await_value(&p).await?;
    let ab = js::call(staging, "getMappedRange", &[js::num(0.0), js::num(len as f64)]).map_err(jserr)?;
    let out = js::bytes_of(&ab);
    let _ = js::call(staging, "unmap", &[]);
    Ok(out)
}

fn staging(device: &JsValue, size: u32) -> Result<JsValue, String> {
    js::call(
        device,
        "createBuffer",
        &[js::obj(&[
            ("size", js::num(align(size.max(4), 4) as f64)),
            ("usage", js::num((buf_usage::MAP_READ | buf_usage::COPY_DST) as f64)),
            ("label", "webgd:readback".into()),
        ])],
    )
    .map_err(jserr)
}

pub async fn read_buffer(device: &JsValue, buffer: &JsValue) -> Result<Vec<u8>, String> {
    let size = js::f(&js::get(buffer, "size")) as u32;
    let usage = js::u(&js::get(buffer, "usage"));
    if js::get(buffer, "mapState").as_string().as_deref() != Some("unmapped") {
        return Err("buffer is mapped".into());
    }
    if usage & buf_usage::MAP_READ != 0 {
        return map_read(buffer, size).await;
    }
    if usage & buf_usage::COPY_SRC == 0 {
        return Err("buffer lacks COPY_SRC".into());
    }
    let st = staging(device, size)?;
    let enc = js::call(device, "createCommandEncoder", &[]).map_err(jserr)?;
    let copy = size & !3;
    if copy > 0 {
        js::call(&enc, "copyBufferToBuffer", &[buffer.clone(), js::num(0.0), st.clone(), js::num(0.0), js::num(copy as f64)])
            .map_err(jserr)?;
    }
    let cb = js::call(&enc, "finish", &[]).map_err(jserr)?;
    js::call(&js::get(device, "queue"), "submit", &[js::arr(&[cb])]).map_err(jserr)?;
    let mut out = map_read(&st, copy).await?;
    out.resize(size as usize, 0);
    let _ = js::call(&st, "destroy", &[]);
    Ok(out)
}

pub async fn read_texture(device: &JsValue, tex: &JsValue, mip: u32, layer: u32) -> Result<Image, String> {
    let format_s = js::get(tex, "format").as_string().unwrap_or_default();
    let pf = gpu_format(&format_s).ok_or_else(|| format!("format not viewable ({format_s})"))?;
    if js::f(&js::get(tex, "sampleCount")) > 1.0 {
        return Err("multisampled texture not readable".into());
    }
    if js::u(&js::get(tex, "usage")) & tex_usage::COPY_SRC == 0 {
        return Err("texture lacks COPY_SRC".into());
    }
    let mips = js::u(&js::get(tex, "mipLevelCount")).max(1);
    if mip >= mips {
        return Err(format!("mip {mip} out of range"));
    }
    let w = (js::u(&js::get(tex, "width")) >> mip).max(1);
    let h = (js::u(&js::get(tex, "height")) >> mip).max(1);
    let aspect = match pf {
        PixelFormat::Depth32Float | PixelFormat::Depth16Unorm => "depth-only",
        PixelFormat::Stencil8 => "stencil-only",
        _ => "all",
    };
    let bpt = pf.bytes_per_texel();
    let row = w * bpt;
    let bpr = align(row, 256);
    let st = staging(device, bpr * h)?;
    let enc = js::call(device, "createCommandEncoder", &[]).map_err(jserr)?;
    js::call(
        &enc,
        "copyTextureToBuffer",
        &[
            js::obj(&[
                ("texture", tex.clone()),
                ("mipLevel", js::num(mip as f64)),
                ("origin", js::obj(&[("x", js::num(0.0)), ("y", js::num(0.0)), ("z", js::num(layer as f64))])),
                ("aspect", aspect.into()),
            ]),
            js::obj(&[("buffer", st.clone()), ("bytesPerRow", js::num(bpr as f64)), ("rowsPerImage", js::num(h as f64))]),
            js::arr(&[js::num(w as f64), js::num(h as f64), js::num(1.0)]),
        ],
    )
    .map_err(jserr)?;
    let cb = js::call(&enc, "finish", &[]).map_err(jserr)?;
    let dev_err = js::call(device, "pushErrorScope", &["validation".into()]);
    js::call(&js::get(device, "queue"), "submit", &[js::arr(&[cb])]).map_err(jserr)?;
    if dev_err.is_ok() {
        let p = js::call(device, "popErrorScope", &[]).map_err(jserr)?;
        let e = await_value(&p).await?;
        if js::is_obj(&e) {
            let _ = js::call(&st, "destroy", &[]);
            return Err(js::err_msg(&e));
        }
    }
    let raw = map_read(&st, bpr * h).await?;
    let _ = js::call(&st, "destroy", &[]);
    let mut data = Vec::with_capacity((row * h) as usize);
    for y in 0..h as usize {
        let s = y * bpr as usize;
        data.extend_from_slice(&raw[s..s + row as usize]);
    }
    Ok(Image { width: w, height: h, format: pf, data })
}
