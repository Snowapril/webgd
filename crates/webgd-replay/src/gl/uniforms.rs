//! Uniform type → setter table.
use crate::js;
use wasm_bindgen::JsValue;

/// Setter method and whether it is a `uniformMatrix*` call.
fn setter(ty: u32) -> Option<(&'static str, bool)> {
    Some(match ty {
        0x1406 => ("uniform1fv", false),
        0x8B50 => ("uniform2fv", false),
        0x8B51 => ("uniform3fv", false),
        0x8B52 => ("uniform4fv", false),
        0x1404 | 0x8B56 => ("uniform1iv", false),
        0x8B53 | 0x8B57 => ("uniform2iv", false),
        0x8B54 | 0x8B58 => ("uniform3iv", false),
        0x8B55 | 0x8B59 => ("uniform4iv", false),
        0x1405 => ("uniform1uiv", false),
        0x8DC6 => ("uniform2uiv", false),
        0x8DC7 => ("uniform3uiv", false),
        0x8DC8 => ("uniform4uiv", false),
        0x8B5A => ("uniformMatrix2fv", true),
        0x8B5B => ("uniformMatrix3fv", true),
        0x8B5C => ("uniformMatrix4fv", true),
        0x8B65 => ("uniformMatrix2x3fv", true),
        0x8B66 => ("uniformMatrix2x4fv", true),
        0x8B67 => ("uniformMatrix3x2fv", true),
        0x8B68 => ("uniformMatrix3x4fv", true),
        0x8B69 => ("uniformMatrix4x2fv", true),
        0x8B6A => ("uniformMatrix4x3fv", true),
        t if is_sampler(t) => ("uniform1iv", false),
        _ => return None,
    })
}

pub fn is_sampler(ty: u32) -> bool {
    matches!(
        ty,
        0x8B5E
            | 0x8B5F
            | 0x8B60
            | 0x8B62
            | 0x8DC1
            | 0x8DC4
            | 0x8DC5
            | 0x8DCA
            | 0x8DCB
            | 0x8DCC
            | 0x8DCF
            | 0x8DD2
            | 0x8DD3
            | 0x8DD4
            | 0x8DD7
    )
}

/// Texture binding pname for a sampler uniform type.
pub fn sampler_binding(ty: u32) -> u32 {
    use super::c::*;
    match ty {
        0x8B5F | 0x8DCB | 0x8DD3 => TEXTURE_BINDING_3D,
        0x8B60 | 0x8DC5 | 0x8DCC | 0x8DD4 => TEXTURE_BINDING_CUBE_MAP,
        0x8DC1 | 0x8DC4 | 0x8DCF | 0x8DD7 => TEXTURE_BINDING_2D_ARRAY,
        _ => TEXTURE_BINDING_2D,
    }
}

/// Sets uniform `loc` of type `ty` from a `getUniform`-shaped value
/// (number, boolean, typed array or array of booleans).
pub fn set_uniform(gl: &JsValue, ty: u32, loc: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    let (method, matrix) =
        setter(ty).ok_or_else(|| JsValue::from_str(&format!("unknown uniform type 0x{ty:X}")))?;
    let v = if js::is_obj(value) {
        if js_sys::Array::is_array(value) {
            // bvec: map booleans to ints.
            let a = js_sys::Array::new();
            for it in js::array_items(value) {
                a.push(&JsValue::from_f64(js::f(&it)));
            }
            a.into()
        } else {
            value.clone()
        }
    } else {
        js::arr(&[JsValue::from_f64(js::as_f64(value).unwrap_or(0.0))])
    };
    if matrix {
        js::call(gl, method, &[loc.clone(), JsValue::FALSE, v])?;
    } else {
        js::call(gl, method, &[loc.clone(), v])?;
    }
    Ok(())
}
