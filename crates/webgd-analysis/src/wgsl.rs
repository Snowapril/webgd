//! WGSL front end: walks naga IR and feeds the cost model.
use std::collections::HashMap;

use naga::{
    BinaryOperator, Block, Expression, Function, Handle, MathFunction, Module, ShaderStage, Statement, Type,
    TypeInner, UniqueArena,
    proc::Layouter,
    valid::{Capabilities, FunctionInfo, ModuleInfo, ValidationFlags, Validator},
};

use crate::model::{self, Cost};
use crate::{AnalysisError, Stage, StageStats};

pub fn stats(src: &str, entry: Option<&str>, stage: Stage) -> Result<StageStats, AnalysisError> {
    let module = naga::front::wgsl::parse_str(src).map_err(|e| AnalysisError::Parse(e.emit_to_string(src)))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .map_err(|e| AnalysisError::Validate(e.emit_to_string(src)))?;

    let want = match stage {
        Stage::Vertex => ShaderStage::Vertex,
        Stage::Fragment => ShaderStage::Fragment,
        Stage::Compute => ShaderStage::Compute,
    };
    let mut candidates = module
        .entry_points
        .iter()
        .enumerate()
        .filter(|(_, ep)| ep.stage == want && entry.is_none_or(|n| ep.name == n));
    let (ep_index, ep) = candidates.next().ok_or(AnalysisError::NoEntry)?;
    if candidates.next().is_some() {
        return Err(AnalysisError::AmbiguousEntry);
    }

    let mut walker = Walker { module: &module, info: &info, memo: HashMap::new(), local_components: 0 };
    let cost = walker.function(&ep.function, info.get_entry_point(ep_index));

    let types = &module.types;
    let io: u32 = ep.function.arguments.iter().map(|a| components(types, a.ty)).sum::<u32>()
        + ep.function.result.as_ref().map_or(0, |r| components(types, r.ty));
    let entry_locals = locals_components(types, &ep.function);

    let (workgroup_size, workgroup_bytes) = if want == ShaderStage::Compute {
        let mut layouter = Layouter::default();
        layouter.update(module.to_ctx()).map_err(|e| AnalysisError::Validate(e.to_string()))?;
        let bytes = module
            .global_variables
            .iter()
            .filter(|(_, g)| g.space == naga::AddressSpace::WorkGroup)
            .map(|(_, g)| layouter[g.ty].size)
            .sum();
        (Some(ep.workgroup_size), bytes)
    } else {
        (None, 0)
    };

    Ok(StageStats {
        alu: cost.alu,
        branches: cost.branches,
        tex_samples: cost.tex,
        est_regs_vec4: model::est_regs_vec4(io + entry_locals + walker.local_components),
        workgroup_size,
        workgroup_bytes,
    })
}

struct Walker<'a> {
    module: &'a Module,
    info: &'a ModuleInfo,
    /// Inlined cost per callee; each callee is analyzed (and its locals counted) once.
    memo: HashMap<Handle<Function>, Cost>,
    /// Local-variable components of every reachable callee.
    local_components: u32,
}

impl Walker<'_> {
    fn function(&mut self, f: &Function, fi: &FunctionInfo) -> Cost {
        self.block(&f.body, f, fi)
    }

    fn call(&mut self, h: Handle<Function>) -> Cost {
        if let Some(c) = self.memo.get(&h) {
            return *c;
        }
        let f = &self.module.functions[h];
        let fi = &self.info[h];
        self.local_components += locals_components(&self.module.types, f);
        let c = self.function(f, fi);
        self.memo.insert(h, c);
        c
    }

    fn block(&mut self, b: &Block, f: &Function, fi: &FunctionInfo) -> Cost {
        let mut c = Cost::ZERO;
        for s in b.iter() {
            c.add(self.statement(s, f, fi));
        }
        c
    }

    fn statement(&mut self, s: &Statement, f: &Function, fi: &FunctionInfo) -> Cost {
        match s {
            Statement::Emit(range) => {
                let mut c = Cost::ZERO;
                for h in range.clone() {
                    c.add(self.expression(h, f, fi));
                }
                c
            }
            Statement::Block(b) => self.block(b, f, fi),
            Statement::If { accept, reject, .. } => {
                model::if_cost(self.block(accept, f, fi), self.block(reject, f, fi))
            }
            Statement::Switch { cases, .. } => {
                let costs: Vec<Cost> = cases.iter().map(|case| self.block(&case.body, f, fi)).collect();
                model::switch_cost(costs)
            }
            Statement::Loop { body, continuing, break_if } => {
                let mut it = self.block(body, f, fi);
                it.add(self.block(continuing, f, fi));
                if break_if.is_some() {
                    it.branches += model::BRANCH;
                }
                model::loop_cost(it)
            }
            Statement::Call { function, .. } => self.call(*function),
            Statement::Atomic { .. } => Cost::alu(1.0),
            _ => Cost::ZERO,
        }
    }

    fn expression(&self, h: Handle<Expression>, f: &Function, fi: &FunctionInfo) -> Cost {
        let types = &self.module.types;
        let ty = |e: Handle<Expression>| fi[e].ty.inner_with(types);
        let w = |e: Handle<Expression>| width(ty(e));
        match &f.expressions[h] {
            Expression::Unary { .. } => Cost::alu(model::unary_cost(w(h))),
            Expression::Binary { op, left, right } => {
                let n = match (op, ty(*left), ty(*right)) {
                    (BinaryOperator::Multiply, TypeInner::Matrix { columns, rows, .. }, TypeInner::Vector { .. })
                    | (BinaryOperator::Multiply, TypeInner::Vector { .. }, TypeInner::Matrix { columns, rows, .. }) => {
                        model::mat_vec_cost(*rows as u32, *columns as u32)
                    }
                    (
                        BinaryOperator::Multiply,
                        TypeInner::Matrix { columns: k, rows: r, .. },
                        TypeInner::Matrix { columns: c, .. },
                    ) => model::mat_mat_cost(*r as u32, *c as u32, *k as u32),
                    _ => model::binary_cost(w(h)),
                };
                Cost::alu(n)
            }
            Expression::Select { .. } => Cost::alu(model::select_cost(w(h))),
            Expression::Relational { argument, .. } => Cost::alu(model::binary_cost(w(*argument))),
            Expression::Derivative { .. } => Cost::alu(model::unary_cost(w(h))),
            Expression::As { .. } => Cost::alu(model::convert_cost(w(h))),
            Expression::Math { fun, arg, .. } => {
                let name = math_name(*fun);
                let width = match fun {
                    MathFunction::Dot | MathFunction::Length | MathFunction::Distance => w(*arg),
                    _ => w(h),
                };
                Cost::alu(model::builtin_cost(name, width))
            }
            Expression::ImageSample { .. } | Expression::ImageLoad { .. } => Cost::sample(),
            _ => Cost::ZERO,
        }
    }
}

/// Lower-case GLSL spelling used by [`model::builtin_cost`].
fn math_name(f: MathFunction) -> &'static str {
    use MathFunction as M;
    match f {
        M::Sqrt => "sqrt",
        M::InverseSqrt => "inversesqrt",
        M::Exp => "exp",
        M::Exp2 => "exp2",
        M::Log => "log",
        M::Log2 => "log2",
        M::Pow => "pow",
        M::Sin => "sin",
        M::Cos => "cos",
        M::Tan => "tan",
        M::Asin => "asin",
        M::Acos => "acos",
        M::Atan => "atan",
        M::Atan2 => "atan2",
        M::Sinh => "sinh",
        M::Cosh => "cosh",
        M::Tanh => "tanh",
        M::Asinh => "asinh",
        M::Acosh => "acosh",
        M::Atanh => "atanh",
        M::Dot => "dot",
        M::Cross => "cross",
        M::Length => "length",
        M::Distance => "distance",
        M::Normalize => "normalize",
        M::Fma => "fma",
        M::Mix => "mix",
        M::Clamp => "clamp",
        M::SmoothStep => "smoothstep",
        _ => "",
    }
}

/// Result component count of a value type (matrices count `rows * cols`); at least 1.
fn width(t: &TypeInner) -> u32 {
    match t {
        TypeInner::Vector { size, .. } => *size as u32,
        TypeInner::Matrix { columns, rows, .. } => *columns as u32 * *rows as u32,
        _ => 1,
    }
}

/// Scalar components held by a value of type `ty` (aggregates summed).
fn components(types: &UniqueArena<Type>, ty: Handle<Type>) -> u32 {
    match &types[ty].inner {
        TypeInner::Scalar(_) | TypeInner::Atomic(_) => 1,
        TypeInner::Vector { size, .. } => *size as u32,
        TypeInner::Matrix { columns, rows, .. } => *columns as u32 * *rows as u32,
        TypeInner::Array { base, size, .. } => {
            let n = match size {
                naga::ArraySize::Constant(n) => n.get(),
                _ => 1,
            };
            n.saturating_mul(components(types, *base))
        }
        TypeInner::Struct { members, .. } => members.iter().map(|m| components(types, m.ty)).sum(),
        _ => 0,
    }
}

fn locals_components(types: &UniqueArena<Type>, f: &Function) -> u32 {
    f.local_variables.iter().map(|(_, v)| components(types, v.ty)).sum()
}
