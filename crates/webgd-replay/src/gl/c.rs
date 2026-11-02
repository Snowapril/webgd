//! WebGL2 enum values used by the replay engine.
#![allow(dead_code)]

pub const NONE: u32 = 0;
pub const BYTE: u32 = 0x1400;
pub const UNSIGNED_BYTE: u32 = 0x1401;
pub const SHORT: u32 = 0x1402;
pub const UNSIGNED_SHORT: u32 = 0x1403;
pub const INT: u32 = 0x1404;
pub const UNSIGNED_INT: u32 = 0x1405;
pub const FLOAT: u32 = 0x1406;
pub const HALF_FLOAT: u32 = 0x140B;
pub const UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;
pub const UNSIGNED_INT_24_8: u32 = 0x84FA;
pub const FLOAT_32_UNSIGNED_INT_24_8_REV: u32 = 0x8DAD;

// formats
pub const DEPTH_COMPONENT: u32 = 0x1902;
pub const RED: u32 = 0x1903;
pub const ALPHA: u32 = 0x1906;
pub const RGB: u32 = 0x1907;
pub const RGBA: u32 = 0x1908;
pub const LUMINANCE: u32 = 0x1909;
pub const LUMINANCE_ALPHA: u32 = 0x190A;
pub const RG: u32 = 0x8227;
pub const RG_INTEGER: u32 = 0x8228;
pub const RED_INTEGER: u32 = 0x8D94;
pub const RGB_INTEGER: u32 = 0x8D98;
pub const RGBA_INTEGER: u32 = 0x8D99;
pub const DEPTH_STENCIL: u32 = 0x84F9;

// internal formats
pub const R8: u32 = 0x8229;
pub const R8_SNORM: u32 = 0x8F94;
pub const R16F: u32 = 0x822D;
pub const R32F: u32 = 0x822E;
pub const R8I: u32 = 0x8231;
pub const R8UI: u32 = 0x8232;
pub const R16I: u32 = 0x8233;
pub const R16UI: u32 = 0x8234;
pub const R32I: u32 = 0x8235;
pub const R32UI: u32 = 0x8236;
pub const RG8: u32 = 0x822B;
pub const RG8_SNORM: u32 = 0x8F95;
pub const RG16F: u32 = 0x822F;
pub const RG32F: u32 = 0x8230;
pub const RG8I: u32 = 0x8237;
pub const RG8UI: u32 = 0x8238;
pub const RG16I: u32 = 0x8239;
pub const RG16UI: u32 = 0x823A;
pub const RG32I: u32 = 0x823B;
pub const RG32UI: u32 = 0x823C;
pub const RGB8: u32 = 0x8051;
pub const SRGB8: u32 = 0x8C41;
pub const RGB565: u32 = 0x8D62;
pub const RGB8_SNORM: u32 = 0x8F96;
pub const R11F_G11F_B10F: u32 = 0x8C3A;
pub const RGB9_E5: u32 = 0x8C3D;
pub const RGB16F: u32 = 0x881B;
pub const RGB32F: u32 = 0x8815;
pub const RGB8UI: u32 = 0x8D7D;
pub const RGB8I: u32 = 0x8D8F;
pub const RGB16UI: u32 = 0x8D77;
pub const RGB16I: u32 = 0x8D89;
pub const RGB32UI: u32 = 0x8D71;
pub const RGB32I: u32 = 0x8D83;
pub const RGBA8: u32 = 0x8058;
pub const SRGB8_ALPHA8: u32 = 0x8C43;
pub const RGBA8_SNORM: u32 = 0x8F97;
pub const RGB5_A1: u32 = 0x8057;
pub const RGBA4: u32 = 0x8056;
pub const RGB10_A2: u32 = 0x8059;
pub const RGBA16F: u32 = 0x881A;
pub const RGBA32F: u32 = 0x8814;
pub const RGBA8UI: u32 = 0x8D7C;
pub const RGBA8I: u32 = 0x8D8E;
pub const RGB10_A2UI: u32 = 0x906F;
pub const RGBA16UI: u32 = 0x8D76;
pub const RGBA16I: u32 = 0x8D88;
pub const RGBA32UI: u32 = 0x8D70;
pub const RGBA32I: u32 = 0x8D82;
pub const DEPTH_COMPONENT16: u32 = 0x81A5;
pub const DEPTH_COMPONENT24: u32 = 0x81A6;
pub const DEPTH_COMPONENT32F: u32 = 0x8CAC;
pub const DEPTH24_STENCIL8: u32 = 0x88F0;
pub const DEPTH32F_STENCIL8: u32 = 0x8CAD;
pub const STENCIL_INDEX8: u32 = 0x8D48;

// capabilities
pub const BLEND: u32 = 0x0BE2;
pub const CULL_FACE: u32 = 0x0B44;
pub const DEPTH_TEST: u32 = 0x0B71;
pub const DITHER: u32 = 0x0BD0;
pub const POLYGON_OFFSET_FILL: u32 = 0x8037;
pub const SAMPLE_ALPHA_TO_COVERAGE: u32 = 0x809E;
pub const SAMPLE_COVERAGE: u32 = 0x80A0;
pub const SCISSOR_TEST: u32 = 0x0C11;
pub const STENCIL_TEST: u32 = 0x0B90;
pub const RASTERIZER_DISCARD: u32 = 0x8C89;
pub const CAPS: [u32; 10] = [
    BLEND,
    CULL_FACE,
    DEPTH_TEST,
    DITHER,
    POLYGON_OFFSET_FILL,
    SAMPLE_ALPHA_TO_COVERAGE,
    SAMPLE_COVERAGE,
    SCISSOR_TEST,
    STENCIL_TEST,
    RASTERIZER_DISCARD,
];

// state
pub const BLEND_COLOR: u32 = 0x8005;
pub const BLEND_EQUATION_RGB: u32 = 0x8009;
pub const BLEND_EQUATION_ALPHA: u32 = 0x883D;
pub const BLEND_SRC_RGB: u32 = 0x80C9;
pub const BLEND_DST_RGB: u32 = 0x80C8;
pub const BLEND_SRC_ALPHA: u32 = 0x80CB;
pub const BLEND_DST_ALPHA: u32 = 0x80CA;
pub const COLOR_WRITEMASK: u32 = 0x0C23;
pub const CULL_FACE_MODE: u32 = 0x0B45;
pub const DEPTH_FUNC: u32 = 0x0B74;
pub const DEPTH_WRITEMASK: u32 = 0x0B72;
pub const DEPTH_RANGE: u32 = 0x0B70;
pub const FRONT_FACE: u32 = 0x0B46;
pub const LINE_WIDTH: u32 = 0x0B21;
pub const POLYGON_OFFSET_FACTOR: u32 = 0x8038;
pub const POLYGON_OFFSET_UNITS: u32 = 0x2A00;
pub const SAMPLE_COVERAGE_VALUE: u32 = 0x80AA;
pub const SAMPLE_COVERAGE_INVERT: u32 = 0x80AB;
pub const SAMPLES: u32 = 0x80A9;
pub const SAMPLE_BUFFERS: u32 = 0x80A8;
pub const SCISSOR_BOX: u32 = 0x0C10;
pub const VIEWPORT: u32 = 0x0BA2;
pub const STENCIL_FUNC: u32 = 0x0B92;
pub const STENCIL_REF: u32 = 0x0B97;
pub const STENCIL_VALUE_MASK: u32 = 0x0B93;
pub const STENCIL_FAIL: u32 = 0x0B94;
pub const STENCIL_PASS_DEPTH_FAIL: u32 = 0x0B95;
pub const STENCIL_PASS_DEPTH_PASS: u32 = 0x0B96;
pub const STENCIL_WRITEMASK: u32 = 0x0B98;
pub const STENCIL_BACK_FUNC: u32 = 0x8800;
pub const STENCIL_BACK_FAIL: u32 = 0x8801;
pub const STENCIL_BACK_PASS_DEPTH_FAIL: u32 = 0x8802;
pub const STENCIL_BACK_PASS_DEPTH_PASS: u32 = 0x8803;
pub const STENCIL_BACK_REF: u32 = 0x8CA3;
pub const STENCIL_BACK_VALUE_MASK: u32 = 0x8CA4;
pub const STENCIL_BACK_WRITEMASK: u32 = 0x8CA5;
pub const COLOR_CLEAR_VALUE: u32 = 0x0C22;
pub const DEPTH_CLEAR_VALUE: u32 = 0x0B73;
pub const STENCIL_CLEAR_VALUE: u32 = 0x0B91;
pub const GENERATE_MIPMAP_HINT: u32 = 0x8192;
pub const FRAGMENT_SHADER_DERIVATIVE_HINT: u32 = 0x8B8B;
pub const FRONT: u32 = 0x0404;
pub const BACK: u32 = 0x0405;

// pixel store
pub const PACK_ALIGNMENT: u32 = 0x0D05;
pub const UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
pub const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
pub const UNPACK_COLORSPACE_CONVERSION_WEBGL: u32 = 0x9243;
pub const BROWSER_DEFAULT_WEBGL: u32 = 0x9244;
pub const PACK_ROW_LENGTH: u32 = 0x0D02;
pub const PACK_SKIP_ROWS: u32 = 0x0D03;
pub const PACK_SKIP_PIXELS: u32 = 0x0D04;
pub const UNPACK_ROW_LENGTH: u32 = 0x0CF2;
pub const UNPACK_IMAGE_HEIGHT: u32 = 0x806E;
pub const UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
pub const UNPACK_SKIP_ROWS: u32 = 0x0CF3;
pub const UNPACK_SKIP_IMAGES: u32 = 0x806D;
pub const PIXEL_STORE: [u32; 13] = [
    PACK_ALIGNMENT,
    UNPACK_ALIGNMENT,
    UNPACK_FLIP_Y_WEBGL,
    UNPACK_PREMULTIPLY_ALPHA_WEBGL,
    UNPACK_COLORSPACE_CONVERSION_WEBGL,
    PACK_ROW_LENGTH,
    PACK_SKIP_ROWS,
    PACK_SKIP_PIXELS,
    UNPACK_ROW_LENGTH,
    UNPACK_IMAGE_HEIGHT,
    UNPACK_SKIP_PIXELS,
    UNPACK_SKIP_ROWS,
    UNPACK_SKIP_IMAGES,
];
/// Default value of each `PIXEL_STORE` entry.
pub const PIXEL_STORE_DEFAULTS: [u32; 13] =
    [4, 4, 0, 0, BROWSER_DEFAULT_WEBGL, 0, 0, 0, 0, 0, 0, 0, 0];

// bindings
pub const ACTIVE_TEXTURE: u32 = 0x84E0;
pub const CURRENT_PROGRAM: u32 = 0x8B8D;
pub const ARRAY_BUFFER_BINDING: u32 = 0x8894;
pub const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
pub const VERTEX_ARRAY_BINDING: u32 = 0x85B5;
pub const DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
pub const READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
pub const RENDERBUFFER_BINDING: u32 = 0x8CA7;
pub const COPY_READ_BUFFER_BINDING: u32 = 0x8F36;
pub const COPY_WRITE_BUFFER_BINDING: u32 = 0x8F37;
pub const PIXEL_PACK_BUFFER_BINDING: u32 = 0x88ED;
pub const PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88EF;
pub const UNIFORM_BUFFER_BINDING: u32 = 0x8A28;
pub const UNIFORM_BUFFER_START: u32 = 0x8A29;
pub const UNIFORM_BUFFER_SIZE: u32 = 0x8A2A;
pub const TRANSFORM_FEEDBACK_BUFFER_BINDING: u32 = 0x8C8F;
pub const TEXTURE_BINDING_2D: u32 = 0x8069;
pub const TEXTURE_BINDING_CUBE_MAP: u32 = 0x8514;
pub const TEXTURE_BINDING_3D: u32 = 0x806A;
pub const TEXTURE_BINDING_2D_ARRAY: u32 = 0x8C1D;
pub const SAMPLER_BINDING: u32 = 0x8919;
pub const MAX_COMBINED_TEXTURE_IMAGE_UNITS: u32 = 0x8B4D;
pub const MAX_DRAW_BUFFERS: u32 = 0x8824;
pub const MAX_VERTEX_ATTRIBS: u32 = 0x8869;
pub const DRAW_BUFFER0: u32 = 0x8825;
pub const READ_BUFFER: u32 = 0x0C02;

// buffer targets
pub const ARRAY_BUFFER: u32 = 0x8892;
pub const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
pub const COPY_READ_BUFFER: u32 = 0x8F36;
pub const COPY_WRITE_BUFFER: u32 = 0x8F37;
pub const PIXEL_PACK_BUFFER: u32 = 0x88EB;
pub const PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
pub const UNIFORM_BUFFER: u32 = 0x8A11;
pub const TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;
pub const BUFFER_SIZE: u32 = 0x8764;

// textures
pub const TEXTURE0: u32 = 0x84C0;
pub const TEXTURE_2D: u32 = 0x0DE1;
pub const TEXTURE_CUBE_MAP: u32 = 0x8513;
pub const TEXTURE_CUBE_MAP_POSITIVE_X: u32 = 0x8515;
pub const TEXTURE_3D: u32 = 0x806F;
pub const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
pub const TEXTURE_MIN_LOD: u32 = 0x813A;
pub const TEXTURE_MAX_LOD: u32 = 0x813B;
pub const TEXTURE_BASE_LEVEL: u32 = 0x813C;
pub const TEXTURE_MAX_LEVEL: u32 = 0x813D;
pub const TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const TEXTURE_COMPARE_MODE: u32 = 0x884C;
pub const UNIFORM_BLOCK_DATA_SIZE: u32 = 0x8A40;
pub const TRIANGLES: u32 = 0x0004;
pub const NEAREST: u32 = 0x2600;

// framebuffers
pub const FRAMEBUFFER: u32 = 0x8D40;
pub const READ_FRAMEBUFFER: u32 = 0x8CA8;
pub const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
pub const RENDERBUFFER: u32 = 0x8D41;
pub const COLOR_ATTACHMENT0: u32 = 0x8CE0;
pub const DEPTH_ATTACHMENT: u32 = 0x8D00;
pub const STENCIL_ATTACHMENT: u32 = 0x8D20;
pub const DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;
pub const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
pub const FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: u32 = 0x8CD0;
pub const FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: u32 = 0x8CD1;
pub const FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL: u32 = 0x8CD2;
pub const FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE: u32 = 0x8CD3;
pub const FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER: u32 = 0x8CD4;
pub const FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE: u32 = 0x8211;
pub const FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING: u32 = 0x8210;
pub const UNSIGNED_NORMALIZED: u32 = 0x8C17;
pub const SRGB: u32 = 0x8C40;
pub const TEXTURE: u32 = 0x1702;
pub const RENDERBUFFER_WIDTH: u32 = 0x8D42;
pub const RENDERBUFFER_HEIGHT: u32 = 0x8D43;
pub const RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;
pub const RENDERBUFFER_SAMPLES: u32 = 0x8CAB;
pub const COLOR_BUFFER_BIT: u32 = 0x4000;
pub const BACK_BUFFER: u32 = 0x0405;

// vertex attribs
pub const VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
pub const VERTEX_ATTRIB_ARRAY_SIZE: u32 = 0x8623;
pub const VERTEX_ATTRIB_ARRAY_STRIDE: u32 = 0x8624;
pub const VERTEX_ATTRIB_ARRAY_TYPE: u32 = 0x8625;
pub const VERTEX_ATTRIB_ARRAY_NORMALIZED: u32 = 0x886A;
pub const VERTEX_ATTRIB_ARRAY_POINTER: u32 = 0x8645;
pub const VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: u32 = 0x889F;
pub const VERTEX_ATTRIB_ARRAY_INTEGER: u32 = 0x88FD;
pub const VERTEX_ATTRIB_ARRAY_DIVISOR: u32 = 0x88FE;

// programs / shaders
pub const VERTEX_SHADER: u32 = 0x8B31;
pub const FRAGMENT_SHADER: u32 = 0x8B30;
pub const SHADER_TYPE: u32 = 0x8B4F;
pub const LINK_STATUS: u32 = 0x8B82;
pub const ACTIVE_UNIFORMS: u32 = 0x8B86;
pub const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
pub const ACTIVE_UNIFORM_BLOCKS: u32 = 0x8A36;
pub const UNIFORM_BLOCK_INDEX: u32 = 0x8A3A;
pub const UNIFORM_BLOCK_BINDING: u32 = 0x8A3F;

// queries
pub const TIME_ELAPSED_EXT: u32 = 0x88BF;
pub const GPU_DISJOINT_EXT: u32 = 0x8FBB;
pub const QUERY_RESULT: u32 = 0x8866;
pub const QUERY_RESULT_AVAILABLE: u32 = 0x8867;

pub const FUNC_ADD: u32 = 0x8006;
pub const ONE: u32 = 1;
