//! Vertex-shader → compute-shader transform (WGSL) and GLSL vertex output discovery, used to
//! capture post-vertex-shader data on webgd's replay device.
//!
//! WGSL IO contract of the generated compute entry (`VsCompute::entry`, `@workgroup_size(64)`), all in
//! bind group `VsCompute::io_group`:
//! - binding 0: `var<storage, read> webgd_in: array<vec4<u32>>` — per record `inputs.len()` vec4 slots,
//!   slot k feeds `@location(inputs[k].location)`; values are raw bits (f32 bits for Float, i32 bits for Sint, u32 for Uint).
//! - binding 1: `var<storage, read> webgd_ids: array<u32>` — per record the `@builtin(vertex_index)` value.
//! - binding 2: `var<uniform> webgd_params: vec4<u32>` — (record count, `@builtin(instance_index)`, 0, 0).
//! - binding 3: `var<storage, read_write> webgd_out: array<vec4<u32>>` — per record `outputs.len()` vec4 slots
//!   of raw bits (Float outputs as f32 bits).
//!
//! Record i is processed by global_invocation_id.x == i (skip when i >= params.x).
//!
//! The transform works on naga IR: the vertex entry becomes a regular function (IO bindings removed,
//! including struct member bindings), every other entry point is dropped, and a compute entry
//! unpacks `webgd_in`, calls the function and packs the results into `webgd_out`. All four IO
//! bindings are always statically used by the generated entry. Nothing else is removed: overrides
//! keep their names and unused original resources stay declared, so `io_group` is one past the
//! highest bind group declared anywhere in the module (0 if none) to avoid binding collisions.
//! Inputs are ordered by location; outputs are the clip-space position first, then `@location`
//! outputs in location order (struct members flattened). Only 32-bit scalar/vector IO is
//! supported; other IO types and builtins other than `vertex_index`/`instance_index` (inputs)
//! and `position` (output) are rejected with [`AnalysisError::Validate`].
use std::collections::{BTreeMap, BTreeSet};

use glsl_lang::ast::{
    DeclarationData, ExternalDeclarationData, StorageQualifierData, TranslationUnit, TypeQualifier,
    TypeQualifierSpecData, TypeSpecifierNonArrayData,
};
use glsl_lang::parse::DefaultParse;
use naga::{
    AddressSpace, ArraySize, BinaryOperator, Binding, Block, BuiltIn, EntryPoint, Expression,
    Function, FunctionArgument, GlobalVariable, Handle, Literal, MemoryDecorations, Module, Range,
    ResourceBinding, Scalar, ShaderStage, Span, Statement, StorageAccess, SwizzleComponent, Type,
    TypeInner, UniqueArena, VectorSize,
    back::wgsl::WriterFlags,
    valid::{Capabilities, ModuleInfo, ValidationFlags, Validator},
};

use crate::AnalysisError;

/// Scalar kind of a shader input/output (webgd-replay maps it to `webgd_core::geometry::ScalarKind`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScalarKind {
    Float,
    Sint,
    Uint,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VsInput {
    pub location: u32,
    pub components: u32,
    pub kind: ScalarKind,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VsOutput {
    /// Output name: struct member name / `out` variable name; "position" for the clip-space position.
    pub name: String,
    pub builtin_position: bool,
    /// `@location(n)` for user outputs (WGSL), None for builtins / GLSL.
    pub location: Option<u32>,
    pub components: u32,
    pub kind: ScalarKind,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VsCompute {
    pub wgsl: String,
    pub entry: String,
    pub io_group: u32,
    pub inputs: Vec<VsInput>,
    pub outputs: Vec<VsOutput>,
    /// Original resource bindings referenced by the transformed code: (group, sorted bindings).
    pub used_groups: Vec<(u32, Vec<u32>)>,
}

/// Name of the generated compute entry point.
const ENTRY: &str = "webgd_vs_main";
/// Workgroup size (x) of the generated compute entry point.
const WORKGROUP_SIZE: u32 = 64;

/// Shape of a 32-bit scalar/vector IO value.
#[derive(Clone, Copy)]
struct IoTy {
    /// `None` for scalars.
    size: Option<VectorSize>,
    kind: ScalarKind,
}

impl IoTy {
    fn components(self) -> u32 {
        self.size.map_or(1, |s| s as u32)
    }
}

/// Where one vertex-entry input value comes from.
#[derive(Clone, Copy)]
enum InSrc {
    Location { location: u32, io: IoTy },
    VertexIndex,
    InstanceIndex,
}

/// How one vertex-entry argument is built.
enum ArgPlan {
    Direct(InSrc),
    Struct {
        ty: Handle<Type>,
        members: Vec<InSrc>,
    },
}

/// One packed output: the function result itself (`member: None`) or one of its struct members.
struct OutSrc {
    member: Option<u32>,
    io: IoTy,
    info: VsOutput,
}

fn unsupported(msg: String) -> AnalysisError {
    AnalysisError::Validate(format!("unsupported vertex shader IO: {msg}"))
}

fn validate(module: &Module, src: &str) -> Result<ModuleInfo, AnalysisError> {
    Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .map_err(|e| AnalysisError::Validate(e.emit_to_string(src)))
}

fn io_ty(types: &UniqueArena<Type>, ty: Handle<Type>, what: &str) -> Result<IoTy, AnalysisError> {
    let (size, scalar) = match types[ty].inner {
        TypeInner::Scalar(s) => (None, s),
        TypeInner::Vector { size, scalar } => (Some(size), scalar),
        _ => {
            return Err(unsupported(format!(
                "{what}: only scalar and vector types are supported"
            )));
        }
    };
    let kind = match (scalar.kind, scalar.width) {
        (naga::ScalarKind::Float, 4) => ScalarKind::Float,
        (naga::ScalarKind::Sint, 4) => ScalarKind::Sint,
        (naga::ScalarKind::Uint, 4) => ScalarKind::Uint,
        _ => {
            return Err(unsupported(format!(
                "{what}: only 32-bit f32/i32/u32 components are supported"
            )));
        }
    };
    Ok(IoTy { size, kind })
}

fn in_src(
    types: &UniqueArena<Type>,
    binding: &Binding,
    ty: Handle<Type>,
    what: &str,
) -> Result<InSrc, AnalysisError> {
    match binding {
        Binding::Location { location, .. } => Ok(InSrc::Location {
            location: *location,
            io: io_ty(types, ty, what)?,
        }),
        Binding::BuiltIn(BuiltIn::VertexIndex) => Ok(InSrc::VertexIndex),
        Binding::BuiltIn(BuiltIn::InstanceIndex) => Ok(InSrc::InstanceIndex),
        Binding::BuiltIn(b) => Err(unsupported(format!("{what}: @builtin({b:?}) input"))),
    }
}

fn out_src(
    types: &UniqueArena<Type>,
    binding: &Binding,
    ty: Handle<Type>,
    member: Option<(u32, Option<&str>)>,
) -> Result<OutSrc, AnalysisError> {
    let what = match member {
        Some((i, name)) => format!(
            "output member `{}`",
            name.map_or(i.to_string(), str::to_string)
        ),
        None => "output".to_string(),
    };
    let (position, location) = match binding {
        Binding::BuiltIn(BuiltIn::Position { .. }) => (true, None),
        Binding::Location { location, .. } => (false, Some(*location)),
        Binding::BuiltIn(b) => {
            return Err(unsupported(format!("{what}: @builtin({b:?}) output")));
        }
    };
    let io = io_ty(types, ty, &what)?;
    let name = if position {
        "position".to_string()
    } else {
        match member.and_then(|(_, n)| n) {
            Some(n) => n.to_string(),
            None => format!("location{}", location.unwrap_or(0)),
        }
    };
    Ok(OutSrc {
        member: member.map(|(i, _)| i),
        io,
        info: VsOutput {
            name,
            builtin_position: position,
            location,
            components: io.components(),
            kind: io.kind,
        },
    })
}

/// Plans how every argument of the vertex entry is fed.
fn plan_args(types: &UniqueArena<Type>, f: &Function) -> Result<Vec<ArgPlan>, AnalysisError> {
    let mut plans = Vec::with_capacity(f.arguments.len());
    for (k, arg) in f.arguments.iter().enumerate() {
        let arg_name = arg.name.clone().unwrap_or_else(|| format!("#{k}"));
        match &arg.binding {
            Some(b) => plans.push(ArgPlan::Direct(in_src(
                types,
                b,
                arg.ty,
                &format!("input `{arg_name}`"),
            )?)),
            None => {
                let TypeInner::Struct { members, .. } = &types[arg.ty].inner else {
                    return Err(unsupported(format!(
                        "input `{arg_name}` has no binding and is not a struct"
                    )));
                };
                let mut srcs = Vec::with_capacity(members.len());
                for (i, m) in members.iter().enumerate() {
                    let what = format!(
                        "input `{arg_name}.{}`",
                        m.name.clone().unwrap_or_else(|| i.to_string())
                    );
                    let b = m
                        .binding
                        .as_ref()
                        .ok_or_else(|| unsupported(format!("{what} has no binding")))?;
                    srcs.push(in_src(types, b, m.ty, &what)?);
                }
                plans.push(ArgPlan::Struct {
                    ty: arg.ty,
                    members: srcs,
                });
            }
        }
    }
    Ok(plans)
}

/// Plans the packed outputs: position first, then `@location` outputs in location order.
fn plan_outputs(types: &UniqueArena<Type>, f: &Function) -> Result<Vec<OutSrc>, AnalysisError> {
    let result = f
        .result
        .as_ref()
        .ok_or_else(|| unsupported("vertex entry returns nothing".to_string()))?;
    let mut outs = Vec::new();
    match &result.binding {
        Some(b) => outs.push(out_src(types, b, result.ty, None)?),
        None => {
            let TypeInner::Struct { members, .. } = &types[result.ty].inner else {
                return Err(unsupported(
                    "result has no binding and is not a struct".to_string(),
                ));
            };
            for (i, m) in members.iter().enumerate() {
                let b = m.binding.as_ref().ok_or_else(|| {
                    unsupported(format!(
                        "output member `{}` has no binding",
                        m.name.clone().unwrap_or_else(|| i.to_string())
                    ))
                })?;
                outs.push(out_src(
                    types,
                    b,
                    m.ty,
                    Some((i as u32, m.name.as_deref())),
                )?);
            }
        }
    }
    outs.sort_by_key(|o| (!o.info.builtin_position, o.info.location));
    Ok(outs)
}

/// Replaces every struct type that carries member IO bindings with a binding-free copy, in place
/// (same handle), so the former entry function is an ordinary function.
fn strip_struct_bindings(module: &mut Module) -> Result<(), AnalysisError> {
    let handles: Vec<Handle<Type>> = module
        .types
        .iter()
        .filter(|(_, t)| {
            matches!(&t.inner, TypeInner::Struct { members, .. }
                if members.iter().any(|m| m.binding.is_some()))
        })
        .map(|(h, _)| h)
        .collect();
    for h in handles {
        let mut t = module.types[h].clone();
        if let TypeInner::Struct { members, .. } = &mut t.inner {
            for m in members {
                m.binding = None;
            }
        }
        if module.types.get(&t).is_some() {
            return Err(AnalysisError::Validate(format!(
                "struct `{}` duplicates an existing type once IO bindings are removed",
                t.name.as_deref().unwrap_or("?")
            )));
        }
        module.types.replace(h, t);
    }
    Ok(())
}

/// Appends expressions to the compute entry, emitting each non-pre-emitted one right away.
struct Builder {
    f: Function,
}

impl Builder {
    fn expr(&mut self, e: Expression) -> Handle<Expression> {
        let pre_emitted = e.needs_pre_emit();
        let h = self.f.expressions.append(e, Span::UNDEFINED);
        if !pre_emitted {
            self.f.body.push(
                Statement::Emit(Range::new_from_bounds(h, h)),
                Span::UNDEFINED,
            );
        }
        h
    }

    fn u32(&mut self, v: u32) -> Handle<Expression> {
        self.expr(Expression::Literal(Literal::U32(v)))
    }

    /// `ptr_base[index]` loaded (pointer to a runtime array element).
    fn load_elem(
        &mut self,
        global: Handle<Expression>,
        index: Handle<Expression>,
    ) -> Handle<Expression> {
        let p = self.expr(Expression::Access {
            base: global,
            index,
        });
        self.expr(Expression::Load { pointer: p })
    }

    /// Reinterprets the first components of a raw `vec4<u32>` as a value of shape `io`.
    fn unpack_raw(&mut self, raw: Handle<Expression>, io: IoTy) -> Handle<Expression> {
        use SwizzleComponent as S;
        let bits = match io.size {
            None => self.expr(Expression::AccessIndex {
                base: raw,
                index: 0,
            }),
            Some(VectorSize::Quad) => raw,
            Some(size) => self.expr(Expression::Swizzle {
                size,
                vector: raw,
                pattern: [S::X, S::Y, S::Z, S::W],
            }),
        };
        match io.kind {
            ScalarKind::Uint => bits,
            ScalarKind::Float => self.expr(Expression::As {
                expr: bits,
                kind: naga::ScalarKind::Float,
                convert: None,
            }),
            ScalarKind::Sint => self.expr(Expression::As {
                expr: bits,
                kind: naga::ScalarKind::Sint,
                convert: None,
            }),
        }
    }

    /// Raw bits of a value of shape `io`, zero-padded to `vec4<u32>`.
    fn pack_raw(
        &mut self,
        v: Handle<Expression>,
        io: IoTy,
        vec4u: Handle<Type>,
        zero: Handle<Expression>,
    ) -> Handle<Expression> {
        let bits = match io.kind {
            ScalarKind::Uint => v,
            ScalarKind::Float | ScalarKind::Sint => self.expr(Expression::As {
                expr: v,
                kind: naga::ScalarKind::Uint,
                convert: None,
            }),
        };
        if io.size == Some(VectorSize::Quad) {
            return bits;
        }
        let mut components = vec![bits];
        components.extend(std::iter::repeat_n(zero, 4 - io.components() as usize));
        self.expr(Expression::Compose {
            ty: vec4u,
            components,
        })
    }
}

fn add_ty(module: &mut Module, inner: TypeInner) -> Handle<Type> {
    module
        .types
        .insert(Type { name: None, inner }, Span::UNDEFINED)
}

fn add_global(
    module: &mut Module,
    name: &str,
    space: AddressSpace,
    group: u32,
    binding: u32,
    ty: Handle<Type>,
) -> Handle<GlobalVariable> {
    module.global_variables.append(
        GlobalVariable {
            name: Some(name.to_string()),
            space,
            binding: Some(ResourceBinding { group, binding }),
            ty,
            init: None,
            memory_decorations: MemoryDecorations::empty(),
        },
        Span::UNDEFINED,
    )
}

/// Builds a compute module that runs vertex entry `entry` (or the single vertex entry) once per record.
pub fn wgsl_vertex_to_compute(src: &str, entry: Option<&str>) -> Result<VsCompute, AnalysisError> {
    let mut module = naga::front::wgsl::parse_str(src)
        .map_err(|e| AnalysisError::Parse(e.emit_to_string(src)))?;
    validate(&module, src)?;

    let mut candidates =
        module.entry_points.iter().enumerate().filter(|(_, ep)| {
            ep.stage == ShaderStage::Vertex && entry.is_none_or(|n| ep.name == n)
        });
    let (ep_index, _) = candidates.next().ok_or(AnalysisError::NoEntry)?;
    if candidates.next().is_some() {
        return Err(AnalysisError::AmbiguousEntry);
    }

    let vs = &module.entry_points[ep_index].function;
    let plans = plan_args(&module.types, vs)?;
    let outs = plan_outputs(&module.types, vs)?;

    // Inputs ordered by location; slot k of a record = k-th input.
    let mut inputs: Vec<VsInput> = plans
        .iter()
        .flat_map(|p| match p {
            ArgPlan::Direct(s) => std::slice::from_ref(s),
            ArgPlan::Struct { members, .. } => members.as_slice(),
        })
        .filter_map(|s| match *s {
            InSrc::Location { location, io } => Some(VsInput {
                location,
                components: io.components(),
                kind: io.kind,
            }),
            InSrc::VertexIndex | InSrc::InstanceIndex => None,
        })
        .collect();
    inputs.sort_by_key(|i| i.location);
    let slots: BTreeMap<u32, u32> = inputs
        .iter()
        .enumerate()
        .map(|(k, i)| (i.location, k as u32))
        .collect();
    let uses_vertex_index = plans.iter().any(|p| match p {
        ArgPlan::Direct(s) => matches!(s, InSrc::VertexIndex),
        ArgPlan::Struct { members, .. } => members.iter().any(|s| matches!(s, InSrc::VertexIndex)),
    });

    let io_group = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| g.binding.as_ref().map(|b| b.group))
        .max()
        .map_or(0, |g| g + 1);

    // The vertex entry becomes a regular function; all entry points are dropped.
    let mut ep = module.entry_points.swap_remove(ep_index);
    module.entry_points.clear();
    module.doc_comments = None;
    for a in &mut ep.function.arguments {
        a.binding = None;
    }
    if let Some(r) = &mut ep.function.result {
        r.binding = None;
    }
    if ep.function.name.is_none() {
        ep.function.name = Some(ep.name.clone());
    }
    strip_struct_bindings(&mut module)?;
    let vs_fn = module.functions.append(ep.function, Span::UNDEFINED);

    // IO resources.
    let t_u32 = add_ty(&mut module, TypeInner::Scalar(Scalar::U32));
    let t_vec3u = add_ty(
        &mut module,
        TypeInner::Vector {
            size: VectorSize::Tri,
            scalar: Scalar::U32,
        },
    );
    let t_vec4u = add_ty(
        &mut module,
        TypeInner::Vector {
            size: VectorSize::Quad,
            scalar: Scalar::U32,
        },
    );
    let t_arr_vec4u = add_ty(
        &mut module,
        TypeInner::Array {
            base: t_vec4u,
            size: ArraySize::Dynamic,
            stride: 16,
        },
    );
    let t_arr_u32 = add_ty(
        &mut module,
        TypeInner::Array {
            base: t_u32,
            size: ArraySize::Dynamic,
            stride: 4,
        },
    );
    let read = AddressSpace::Storage {
        access: StorageAccess::LOAD,
    };
    let g_in = add_global(&mut module, "webgd_in", read, io_group, 0, t_arr_vec4u);
    let g_ids = add_global(&mut module, "webgd_ids", read, io_group, 1, t_arr_u32);
    let g_params = add_global(
        &mut module,
        "webgd_params",
        AddressSpace::Uniform,
        io_group,
        2,
        t_vec4u,
    );
    let g_out = add_global(
        &mut module,
        "webgd_out",
        AddressSpace::Storage {
            access: StorageAccess::LOAD | StorageAccess::STORE,
        },
        io_group,
        3,
        t_arr_vec4u,
    );

    // Compute entry body.
    let mut b = Builder {
        f: Function {
            name: Some(ENTRY.to_string()),
            arguments: vec![FunctionArgument {
                name: Some("webgd_gid".to_string()),
                ty: t_vec3u,
                binding: Some(Binding::BuiltIn(BuiltIn::GlobalInvocationId)),
            }],
            ..Function::default()
        },
    };
    let e_in = b.expr(Expression::GlobalVariable(g_in));
    let e_ids = b.expr(Expression::GlobalVariable(g_ids));
    let e_params = b.expr(Expression::GlobalVariable(g_params));
    let e_out = b.expr(Expression::GlobalVariable(g_out));
    let gid = b.expr(Expression::FunctionArgument(0));
    let i = b.expr(Expression::AccessIndex {
        base: gid,
        index: 0,
    });
    let params = b.expr(Expression::Load { pointer: e_params });
    let count = b.expr(Expression::AccessIndex {
        base: params,
        index: 0,
    });
    let past_end = b.expr(Expression::Binary {
        op: BinaryOperator::GreaterEqual,
        left: i,
        right: count,
    });
    let mut accept = Block::new();
    accept.push(Statement::Return { value: None }, Span::UNDEFINED);
    b.f.body.push(
        Statement::If {
            condition: past_end,
            accept,
            reject: Block::new(),
        },
        Span::UNDEFINED,
    );

    // Keep every IO binding statically used (pipelines may use an auto layout).
    if inputs.is_empty() {
        let len = b.expr(Expression::ArrayLength(e_in));
        b.f.named_expressions
            .insert(len, "webgd_in_len".to_string());
    }
    if !uses_vertex_index {
        let len = b.expr(Expression::ArrayLength(e_ids));
        b.f.named_expressions
            .insert(len, "webgd_ids_len".to_string());
    }

    let instance = b.expr(Expression::AccessIndex {
        base: params,
        index: 1,
    });
    let in_base = if inputs.is_empty() {
        None
    } else {
        let n = b.u32(inputs.len() as u32);
        Some(b.expr(Expression::Binary {
            op: BinaryOperator::Multiply,
            left: i,
            right: n,
        }))
    };
    let mut vertex_index = None;
    let mut value_of = |b: &mut Builder, s: InSrc| -> Handle<Expression> {
        match s {
            InSrc::Location { location, io } => {
                let base = in_base.expect("location inputs imply a record base");
                let k = b.u32(slots[&location]);
                let idx = b.expr(Expression::Binary {
                    op: BinaryOperator::Add,
                    left: base,
                    right: k,
                });
                let raw = b.load_elem(e_in, idx);
                b.unpack_raw(raw, io)
            }
            InSrc::VertexIndex => *vertex_index.get_or_insert_with(|| b.load_elem(e_ids, i)),
            InSrc::InstanceIndex => instance,
        }
    };
    let mut args = Vec::with_capacity(plans.len());
    for plan in &plans {
        let arg = match plan {
            ArgPlan::Direct(s) => value_of(&mut b, *s),
            ArgPlan::Struct { ty, members } => {
                let components = members.iter().map(|s| value_of(&mut b, *s)).collect();
                b.expr(Expression::Compose {
                    ty: *ty,
                    components,
                })
            }
        };
        args.push(arg);
    }

    // `plan_outputs` guarantees a result; the call result is defined by the `Call` (never emitted).
    let result =
        b.f.expressions
            .append(Expression::CallResult(vs_fn), Span::UNDEFINED);
    b.f.body.push(
        Statement::Call {
            function: vs_fn,
            arguments: args,
            result: Some(result),
        },
        Span::UNDEFINED,
    );

    let m = b.u32(outs.len() as u32);
    let out_base = b.expr(Expression::Binary {
        op: BinaryOperator::Multiply,
        left: i,
        right: m,
    });
    let zero = b.u32(0);
    for (j, o) in outs.iter().enumerate() {
        let v = match o.member {
            Some(index) => b.expr(Expression::AccessIndex {
                base: result,
                index,
            }),
            None => result,
        };
        let packed = b.pack_raw(v, o.io, t_vec4u, zero);
        let k = b.u32(j as u32);
        let idx = b.expr(Expression::Binary {
            op: BinaryOperator::Add,
            left: out_base,
            right: k,
        });
        let p = b.expr(Expression::Access {
            base: e_out,
            index: idx,
        });
        b.f.body.push(
            Statement::Store {
                pointer: p,
                value: packed,
            },
            Span::UNDEFINED,
        );
    }

    module.entry_points.push(EntryPoint {
        name: ENTRY.to_string(),
        stage: ShaderStage::Compute,
        early_depth_test: None,
        workgroup_size: [WORKGROUP_SIZE, 1, 1],
        workgroup_size_overrides: None,
        function: b.f,
        mesh_info: None,
        task_payload: None,
        incoming_ray_payload: None,
    });

    let info = validate(&module, src)?;
    let ep_info = info.get_entry_point(0);
    let mut used: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for (h, g) in module.global_variables.iter() {
        if let Some(rb) = &g.binding
            && rb.group != io_group
            && !ep_info[h].is_empty()
        {
            used.entry(rb.group).or_default().insert(rb.binding);
        }
    }
    let used_groups = used
        .into_iter()
        .map(|(g, b)| (g, b.into_iter().collect()))
        .collect();

    let wgsl = naga::back::wgsl::write_string(&module, &info, WriterFlags::empty())
        .map_err(|e| AnalysisError::Validate(format!("WGSL output failed: {e}")))?;

    // The emitted text must stand on its own.
    let reparsed = naga::front::wgsl::parse_str(&wgsl).map_err(|e| {
        AnalysisError::Validate(format!(
            "generated WGSL does not parse: {}",
            e.emit_to_string(&wgsl)
        ))
    })?;
    validate(&reparsed, &wgsl)
        .map_err(|e| AnalysisError::Validate(format!("generated WGSL does not validate: {e}")))?;
    let entry = reparsed
        .entry_points
        .iter()
        .find(|ep| ep.stage == ShaderStage::Compute)
        .map(|ep| ep.name.clone())
        .ok_or_else(|| {
            AnalysisError::Validate("generated WGSL lost its compute entry".to_string())
        })?;

    Ok(VsCompute {
        wgsl,
        entry,
        io_group,
        inputs,
        outputs: outs.into_iter().map(|o| o.info).collect(),
        used_groups,
    })
}

/// Blank out preprocessor lines (`#version`, `#define`, ...), as the GLSL stats front end does.
fn strip_preprocessor(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if !line.trim_start().starts_with('#') {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn is_out(q: Option<&TypeQualifier>) -> bool {
    q.iter().flat_map(|q| q.qualifiers.iter()).any(|spec| {
        matches!(&spec.content, TypeQualifierSpecData::Storage(st)
            if matches!(st.content, StorageQualifierData::Out))
    })
}

/// Component count and kind of a GLSL scalar/vector type; `None` for everything else.
fn glsl_io_ty(t: &TypeSpecifierNonArrayData) -> Option<(u32, ScalarKind)> {
    use TypeSpecifierNonArrayData as T;
    Some(match t {
        T::Float => (1, ScalarKind::Float),
        T::Vec2 => (2, ScalarKind::Float),
        T::Vec3 => (3, ScalarKind::Float),
        T::Vec4 => (4, ScalarKind::Float),
        T::Int => (1, ScalarKind::Sint),
        T::IVec2 => (2, ScalarKind::Sint),
        T::IVec3 => (3, ScalarKind::Sint),
        T::IVec4 => (4, ScalarKind::Sint),
        T::UInt => (1, ScalarKind::Uint),
        T::UVec2 => (2, ScalarKind::Uint),
        T::UVec3 => (3, ScalarKind::Uint),
        T::UVec4 => (4, ScalarKind::Uint),
        _ => return None,
    })
}

/// Transform-feedback-capturable outputs of a GLSL ES 3.00 vertex shader: "gl_Position" first
/// (builtin_position = true, 4 × Float), then each top-level `out` variable of scalar/vector type in declaration order.
///
/// `out` variables of array, matrix, struct or interface-block type are not reported (no output
/// entry is produced for them).
pub fn glsl_vertex_outputs(src: &str) -> Result<Vec<VsOutput>, AnalysisError> {
    let src = strip_preprocessor(src);
    let tu =
        TranslationUnit::parse(src.as_str()).map_err(|e| AnalysisError::Parse(e.to_string()))?;
    let mut outs = vec![VsOutput {
        name: "gl_Position".to_string(),
        builtin_position: true,
        location: None,
        components: 4,
        kind: ScalarKind::Float,
    }];
    for ed in &tu.0 {
        let ExternalDeclarationData::Declaration(d) = &ed.content else {
            continue;
        };
        let DeclarationData::InitDeclaratorList(l) = &d.content else {
            continue;
        };
        if !is_out(l.head.ty.qualifier.as_ref()) || l.head.ty.ty.array_specifier.is_some() {
            continue;
        }
        let Some((components, kind)) = glsl_io_ty(&l.head.ty.ty.ty.content) else {
            continue;
        };
        let mut push = |name: &str| {
            outs.push(VsOutput {
                name: name.to_string(),
                builtin_position: false,
                location: None,
                components,
                kind,
            })
        };
        if let Some(n) = &l.head.name
            && l.head.array_specifier.is_none()
        {
            push(n.0.as_str());
        }
        for t in &l.tail {
            if t.ident.array_spec.is_none() {
                push(t.ident.ident.0.as_str());
            }
        }
    }
    Ok(outs)
}
