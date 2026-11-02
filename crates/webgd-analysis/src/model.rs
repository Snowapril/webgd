//! Illustrative generic GPU cost model.
//!
//! None of these numbers describe a real GPU. They model a generic SIMT
//! machine well enough to rank shaders against each other: every constant
//! and formula that feeds the "(est.)" counters lives in this file, so the
//! WGSL and GLSL front ends only classify operations and report widths.
//!
//! * ALU cost is measured in scalar operations. `W` is the component count of
//!   the operation's result (matrices count `rows * cols`).
//! * Branches count control-flow decisions (`if`, `switch`, loop exit checks).
//! * Texture samples count image sample/load operations.
//! * Loops are assumed to run [`LOOP_TRIPS`] times.
use crate::StageStats;

/// Assumed trip count of every loop.
pub const LOOP_TRIPS: f64 = 8.0;
/// Branch count added per `if` / `switch` / loop exit check.
pub const BRANCH: f64 = 1.0;
/// Fixed register overhead (in vec4 registers) added to every stage.
pub const REG_OVERHEAD_VEC4: u32 = 2;
/// Maximum resident waves per SIMD.
pub const MAX_WAVES: u32 = 16;
/// Scalar register file per SIMD lane group, in 32-bit registers.
pub const REG_FILE_SCALARS: u32 = 512;
/// Minimum register allocation per wave, in 32-bit registers.
pub const MIN_REG_ALLOC: u32 = 16;
/// Workgroup (LDS) memory per SIMD, in bytes.
pub const LDS_BYTES: u32 = 65536;
/// Threads per wave.
pub const WAVE_WIDTH: u32 = 32;

/// Accumulated static cost of a block of shader code.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub alu: f64,
    pub branches: f64,
    pub tex: f64,
}

impl Cost {
    pub const ZERO: Cost = Cost { alu: 0.0, branches: 0.0, tex: 0.0 };

    pub fn alu(n: f64) -> Cost {
        Cost { alu: n, ..Cost::ZERO }
    }

    /// One texture sample/load.
    pub fn sample() -> Cost {
        Cost { tex: 1.0, ..Cost::ZERO }
    }

    pub fn add(&mut self, o: Cost) {
        self.alu += o.alu;
        self.branches += o.branches;
        self.tex += o.tex;
    }

    /// Component-wise maximum (the more expensive side of a branch).
    pub fn max(self, o: Cost) -> Cost {
        Cost { alu: self.alu.max(o.alu), branches: self.branches.max(o.branches), tex: self.tex.max(o.tex) }
    }

    pub fn scale(self, k: f64) -> Cost {
        Cost { alu: self.alu * k, branches: self.branches * k, tex: self.tex * k }
    }
}

/// `if`: one branch plus the more expensive side. The condition is costed by the caller.
pub fn if_cost(accept: Cost, reject: Cost) -> Cost {
    let mut c = accept.max(reject);
    c.branches += BRANCH;
    c
}

/// `switch`: one branch plus the most expensive case.
pub fn switch_cost(cases: impl IntoIterator<Item = Cost>) -> Cost {
    let mut c = cases.into_iter().fold(Cost::ZERO, Cost::max);
    c.branches += BRANCH;
    c
}

/// Loop: one iteration (body, condition and the exit check, which counts as a branch) times [`LOOP_TRIPS`].
pub fn loop_cost(mut iteration: Cost) -> Cost {
    iteration.branches += BRANCH;
    iteration.scale(LOOP_TRIPS)
}

/// Unary arithmetic/bitwise/logical op with result width `w`.
pub fn unary_cost(w: u32) -> f64 {
    w as f64
}

/// Binary arithmetic/bitwise/comparison op with result width `w`.
pub fn binary_cost(w: u32) -> f64 {
    w as f64
}

/// `select` / ternary with result width `w`.
pub fn select_cost(w: u32) -> f64 {
    w as f64
}

/// Type conversion with result width `w`.
pub fn convert_cost(w: u32) -> f64 {
    w as f64
}

/// Matrix (`rows x cols`) times vector, or vector times matrix.
pub fn mat_vec_cost(rows: u32, cols: u32) -> f64 {
    (rows * cols) as f64
}

/// Matrix (`r x k`) times matrix (`k x c`).
pub fn mat_mat_cost(r: u32, c: u32, k: u32) -> f64 {
    (r * c * k) as f64
}

/// Built-in function cost. `name` is the lower-case GLSL spelling (WGSL
/// built-ins are mapped onto it). `w` is the result width, except for
/// `dot`, `length` and `distance`, whose `w` is the argument width.
pub fn builtin_cost(name: &str, w: u32) -> f64 {
    let w = w as f64;
    match name {
        "dot" => w,
        "cross" => 6.0,
        "length" | "distance" => w + 4.0,
        "normalize" => 3.0 * w + 4.0,
        "sqrt" | "inversesqrt" | "exp" | "exp2" | "log" | "log2" | "pow" | "sin" | "cos" | "tan" | "asin"
        | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" => 4.0 * w,
        "fma" => w,
        "mix" => 3.0 * w,
        "clamp" => 2.0 * w,
        "smoothstep" => 6.0 * w,
        _ => w,
    }
}

/// Estimated vec4 registers: live scalar components (function-local
/// variables of the entry point and its callees, entry inputs, entry
/// outputs) packed into vec4s, plus [`REG_OVERHEAD_VEC4`].
pub fn est_regs_vec4(components: u32) -> u32 {
    components.div_ceil(4) + REG_OVERHEAD_VEC4
}

/// Estimated occupancy in `0..=1`:
/// `min(16, floor(512 / max(16, regs*4)), lds_waves) / 16`, where for compute
/// stages `lds_waves = floor(65536 / max(1, workgroup_bytes)) * ceil(threads / 32)`
/// and 16 otherwise.
pub fn occupancy(stats: &StageStats) -> f64 {
    let reg_waves = REG_FILE_SCALARS / MIN_REG_ALLOC.max(stats.est_regs_vec4.saturating_mul(4));
    let lds_waves = match stats.workgroup_size {
        Some([x, y, z]) => {
            let threads = (x as u64 * y as u64 * z as u64).max(1);
            let groups = LDS_BYTES as u64 / stats.workgroup_bytes.max(1) as u64;
            groups.saturating_mul(threads.div_ceil(WAVE_WIDTH as u64))
        }
        None => MAX_WAVES as u64,
    };
    let waves = (MAX_WAVES as u64).min(reg_waves as u64).min(lds_waves);
    waves as f64 / MAX_WAVES as f64
}
