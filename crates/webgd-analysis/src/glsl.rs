//! GLSL (ES 3.00) front end: walks the glsl-lang AST with a small width
//! inference and feeds the cost model.
use std::collections::{HashMap, HashSet};

use glsl_lang::ast::{
    ArraySpecifier, ArraySpecifierDimensionData, AssignmentOpData, BinaryOpData, BlockData,
    ConditionData, DeclarationData, ExprData, ExternalDeclarationData, ForInitStatementData,
    FunIdentifierData, FunctionDefinition, FunctionParameterDeclarationData, Initializer,
    InitializerData, IterationStatementData, JumpStatementData, LayoutQualifierSpecData,
    SelectionRestStatementData, SingleDeclaration, StatementData, StorageQualifierData,
    StructFieldSpecifier, TranslationUnit, TypeQualifier, TypeQualifierSpecData, TypeSpecifier,
    TypeSpecifierNonArrayData, UnaryOpData,
};
use glsl_lang::parse::DefaultParse;

use crate::model::{self, Cost};
use crate::{AnalysisError, Stage, StageStats};

/// Inferred value type: numbers carry their shape, structs their name.
#[derive(Clone, Debug, PartialEq)]
enum Ty {
    /// `rows x cols`: scalar 1x1, vecN Nx1, matCxR RxC.
    Num {
        rows: u32,
        cols: u32,
    },
    Struct(String),
    Array(Box<Ty>, u32),
    Other,
}

const SCALAR: Ty = Ty::Num { rows: 1, cols: 1 };

impl Ty {
    fn vec(n: u32) -> Ty {
        Ty::Num { rows: n, cols: 1 }
    }
}

/// Blank out preprocessor lines (`#version`, `#define`, ...): the min lexer does not handle them.
fn preprocess(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if !line.trim_start().starts_with('#') {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

pub fn stats(src: &str, stage: Stage) -> Result<StageStats, AnalysisError> {
    let src = preprocess(src);
    let tu =
        TranslationUnit::parse(src.as_str()).map_err(|e| AnalysisError::Parse(e.to_string()))?;

    let mut a = Analyzer::default();
    for ed in &tu.0 {
        match &ed.content {
            ExternalDeclarationData::FunctionDefinition(fd) => {
                a.functions
                    .entry(fd.prototype.name.0.to_string())
                    .or_default()
                    .push(fd);
            }
            ExternalDeclarationData::Declaration(d) => a.global_declaration(&d.content, stage),
            ExternalDeclarationData::Preprocessor(_) => {}
        }
    }
    let main = a
        .functions
        .get("main")
        .and_then(|v| {
            v.iter()
                .find(|f| f.prototype.parameters.is_empty())
                .copied()
        })
        .ok_or(AnalysisError::NoEntry)?;
    let cost = a.call_def(main);
    Ok(StageStats {
        alu: cost.alu,
        branches: cost.branches,
        tex_samples: cost.tex,
        est_regs_vec4: model::est_regs_vec4(a.io_components + a.local_components),
        workgroup_size: (stage == Stage::Compute).then_some(a.local_size),
        workgroup_bytes: if stage == Stage::Compute {
            a.shared_bytes
        } else {
            0
        },
    })
}

struct Analyzer<'a> {
    structs: HashMap<String, Vec<(String, Ty)>>,
    globals: HashMap<String, Ty>,
    const_ints: HashMap<String, u32>,
    functions: HashMap<String, Vec<&'a FunctionDefinition>>,
    /// Inlined cost per function definition (by address); each analyzed once.
    memo: HashMap<*const FunctionDefinition, Cost>,
    in_progress: HashSet<*const FunctionDefinition>,
    scopes: Vec<HashMap<String, Ty>>,
    io_components: u32,
    local_components: u32,
    shared_bytes: u32,
    local_size: [u32; 3],
}

impl Default for Analyzer<'_> {
    fn default() -> Self {
        Analyzer {
            structs: HashMap::new(),
            globals: HashMap::new(),
            const_ints: HashMap::new(),
            functions: HashMap::new(),
            memo: HashMap::new(),
            in_progress: HashSet::new(),
            scopes: Vec::new(),
            io_components: 0,
            local_components: 0,
            shared_bytes: 0,
            local_size: [1, 1, 1],
        }
    }
}

#[derive(Default)]
struct Storage {
    io: bool,
    shared: bool,
    constant: bool,
}

fn storage(q: Option<&TypeQualifier>, stage: Stage) -> Storage {
    let mut s = Storage::default();
    for spec in q.iter().flat_map(|q| q.qualifiers.iter()) {
        if let TypeQualifierSpecData::Storage(st) = &spec.content {
            match &st.content {
                StorageQualifierData::In
                | StorageQualifierData::Out
                | StorageQualifierData::Attribute => s.io = true,
                StorageQualifierData::Varying => s.io = stage != Stage::Compute,
                StorageQualifierData::Shared => s.shared = true,
                StorageQualifierData::Const => s.constant = true,
                _ => {}
            }
        }
    }
    s
}

impl<'a> Analyzer<'a> {
    // ---- types ----

    fn components(&self, t: &Ty) -> u32 {
        match t {
            Ty::Num { rows, cols } => rows * cols,
            Ty::Struct(n) => self
                .structs
                .get(n)
                .map_or(0, |f| f.iter().map(|(_, t)| self.components(t)).sum()),
            Ty::Array(e, n) => n.saturating_mul(self.components(e)),
            Ty::Other => 0,
        }
    }

    /// Operation width: component count, at least 1.
    fn width(&self, t: &Ty) -> u32 {
        self.components(t).max(1)
    }

    fn array_len(&self, spec: &ArraySpecifier) -> u32 {
        spec.dimensions
            .iter()
            .map(|d| match &d.content {
                ArraySpecifierDimensionData::ExplicitlySized(e) => self.const_int(e).unwrap_or(1),
                ArraySpecifierDimensionData::Unsized => 1,
            })
            .product()
    }

    fn const_int(&self, e: &ExprData) -> Option<u32> {
        match e {
            ExprData::IntConst(i) => u32::try_from(*i).ok(),
            ExprData::UIntConst(u) => Some(*u),
            ExprData::Variable(id) => self.const_ints.get(id.0.as_str()).copied(),
            _ => None,
        }
    }

    fn with_array(&self, t: Ty, spec: Option<&ArraySpecifier>) -> Ty {
        match spec {
            Some(s) => Ty::Array(Box::new(t), self.array_len(s)),
            None => t,
        }
    }

    fn type_spec(&mut self, ts: &TypeSpecifier) -> Ty {
        use TypeSpecifierNonArrayData as T;
        let base = match &ts.ty.content {
            T::Float | T::Int | T::UInt | T::Bool | T::Double => SCALAR,
            T::Vec2 | T::IVec2 | T::UVec2 | T::BVec2 | T::DVec2 => Ty::vec(2),
            T::Vec3 | T::IVec3 | T::UVec3 | T::BVec3 | T::DVec3 => Ty::vec(3),
            T::Vec4 | T::IVec4 | T::UVec4 | T::BVec4 | T::DVec4 => Ty::vec(4),
            T::Mat2 | T::Mat22 | T::DMat2 | T::DMat22 => Ty::Num { rows: 2, cols: 2 },
            T::Mat3 | T::Mat33 | T::DMat3 | T::DMat33 => Ty::Num { rows: 3, cols: 3 },
            T::Mat4 | T::Mat44 | T::DMat4 | T::DMat44 => Ty::Num { rows: 4, cols: 4 },
            T::Mat23 | T::DMat23 => Ty::Num { rows: 3, cols: 2 },
            T::Mat24 | T::DMat24 => Ty::Num { rows: 4, cols: 2 },
            T::Mat32 | T::DMat32 => Ty::Num { rows: 2, cols: 3 },
            T::Mat34 | T::DMat34 => Ty::Num { rows: 4, cols: 3 },
            T::Mat42 | T::DMat42 => Ty::Num { rows: 2, cols: 4 },
            T::Mat43 | T::DMat43 => Ty::Num { rows: 3, cols: 4 },
            T::Struct(s) => {
                let fields = self.fields(&s.fields);
                match &s.name {
                    Some(n) => {
                        let name = n.0.to_string();
                        self.structs.insert(name.clone(), fields);
                        Ty::Struct(name)
                    }
                    None => {
                        let name = format!("<anon{}>", self.structs.len());
                        self.structs.insert(name.clone(), fields);
                        Ty::Struct(name)
                    }
                }
            }
            T::TypeName(n) if self.structs.contains_key(n.0.as_str()) => {
                Ty::Struct(n.0.to_string())
            }
            _ => Ty::Other,
        };
        self.with_array(base, ts.array_specifier.as_ref())
    }

    fn fields(&mut self, fields: &[StructFieldSpecifier]) -> Vec<(String, Ty)> {
        let mut out = Vec::new();
        for f in fields {
            let t = self.type_spec(&f.ty);
            for id in &f.identifiers {
                out.push((
                    id.ident.0.to_string(),
                    self.with_array(t.clone(), id.array_spec.as_ref()),
                ));
            }
        }
        out
    }

    // ---- globals ----

    fn global_declaration(&mut self, d: &DeclarationData, stage: Stage) {
        match d {
            DeclarationData::InitDeclaratorList(l) => {
                let st = storage(l.head.ty.qualifier.as_ref(), stage);
                let base = self.type_spec(&l.head.ty.ty);
                let mut decls: Vec<(String, Ty, Option<&Initializer>)> = Vec::new();
                if let Some(n) = &l.head.name {
                    decls.push((
                        n.0.to_string(),
                        self.with_array(base.clone(), l.head.array_specifier.as_ref()),
                        l.head.initializer.as_ref(),
                    ));
                }
                for t in &l.tail {
                    decls.push((
                        t.ident.ident.0.to_string(),
                        self.with_array(base.clone(), t.ident.array_spec.as_ref()),
                        t.initializer.as_ref(),
                    ));
                }
                for (name, ty, init) in decls {
                    let comps = self.components(&ty);
                    if st.io {
                        self.io_components += comps;
                    }
                    if st.shared {
                        self.shared_bytes += 4 * comps;
                    }
                    if st.constant
                        && let Some(InitializerData::Simple(e)) = init.map(|i| &i.content)
                        && let Some(v) = self.const_int(e)
                    {
                        self.const_ints.insert(name.clone(), v);
                    }
                    self.globals.insert(name, ty);
                }
            }
            DeclarationData::Block(b) => self.block_decl(b, stage),
            DeclarationData::TypeOnly(q) => {
                for spec in &q.qualifiers {
                    if let TypeQualifierSpecData::Layout(l) = &spec.content {
                        for id in &l.ids {
                            if let LayoutQualifierSpecData::Identifier(name, Some(e)) = &id.content
                            {
                                let axis = match name.0.as_str() {
                                    "local_size_x" => 0,
                                    "local_size_y" => 1,
                                    "local_size_z" => 2,
                                    _ => continue,
                                };
                                if let Some(v) = self.const_int(e) {
                                    self.local_size[axis] = v;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn block_decl(&mut self, b: &BlockData, stage: Stage) {
        let st = storage(Some(&b.qualifier), stage);
        let fields = self.fields(&b.fields);
        let block_name = b.name.0.to_string();
        self.structs.insert(block_name.clone(), fields.clone());
        let ty = match &b.identifier {
            Some(id) => {
                let t = self.with_array(Ty::Struct(block_name), id.array_spec.as_ref());
                self.globals.insert(id.ident.0.to_string(), t.clone());
                t
            }
            None => {
                for (n, t) in &fields {
                    self.globals.insert(n.clone(), t.clone());
                }
                Ty::Struct(block_name)
            }
        };
        if st.io {
            self.io_components += self.components(&ty);
        }
    }

    // ---- functions ----

    fn lookup(&self, name: &str) -> Ty {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .or_else(|| self.globals.get(name))
            .cloned()
            .unwrap_or_else(|| builtin_var(name))
    }

    fn declare(&mut self, name: String, ty: Ty) {
        self.local_components += self.components(&ty);
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name, ty);
        }
    }

    fn call_def(&mut self, fd: &'a FunctionDefinition) -> Cost {
        let key = fd as *const FunctionDefinition;
        if let Some(c) = self.memo.get(&key) {
            return *c;
        }
        if !self.in_progress.insert(key) {
            return Cost::ZERO; // recursion is invalid GLSL; do not loop.
        }
        let saved = std::mem::take(&mut self.scopes);
        let mut params = HashMap::new();
        for p in &fd.prototype.parameters {
            if let FunctionParameterDeclarationData::Named(_, d) = &p.content {
                let t = self.type_spec(&d.ty);
                params.insert(
                    d.ident.ident.0.to_string(),
                    self.with_array(t, d.ident.array_spec.as_ref()),
                );
            }
        }
        self.scopes.push(params);
        let mut c = Cost::ZERO;
        for s in &fd.statement.statement_list {
            c.add(self.statement(s));
        }
        self.scopes = saved;
        self.in_progress.remove(&key);
        self.memo.insert(key, c);
        c
    }

    fn return_type(&mut self, fd: &FunctionDefinition) -> Ty {
        self.type_spec(&fd.prototype.ty.ty)
    }

    // ---- statements ----

    fn scoped(&mut self, f: impl FnOnce(&mut Self) -> Cost) -> Cost {
        self.scopes.push(HashMap::new());
        let c = f(self);
        self.scopes.pop();
        c
    }

    fn statement(&mut self, s: &StatementData) -> Cost {
        match s {
            StatementData::Declaration(d) => self.local_declaration(d),
            StatementData::Expression(e) => e.0.as_ref().map_or(Cost::ZERO, |e| self.expr(e).0),
            StatementData::Selection(sel) => {
                let mut c = self.expr(&sel.cond).0;
                let (accept, reject) = match &sel.rest.content {
                    SelectionRestStatementData::Statement(a) => {
                        (self.scoped(|s| s.statement(a)), Cost::ZERO)
                    }
                    SelectionRestStatementData::Else(a, r) => (
                        self.scoped(|s| s.statement(a)),
                        self.scoped(|s| s.statement(r)),
                    ),
                };
                c.add(model::if_cost(accept, reject));
                c
            }
            StatementData::Switch(sw) => {
                let mut c = self.expr(&sw.head).0;
                let mut cases = Vec::new();
                let mut cur: Option<Cost> = None;
                self.scopes.push(HashMap::new());
                for st in &sw.body {
                    if let StatementData::CaseLabel(_) = &st.content {
                        cases.extend(cur.take());
                        cur = Some(Cost::ZERO);
                    } else {
                        let sc = self.statement(st);
                        cur.get_or_insert(Cost::ZERO).add(sc);
                    }
                }
                self.scopes.pop();
                cases.extend(cur);
                c.add(model::switch_cost(cases));
                c
            }
            StatementData::CaseLabel(_) => Cost::ZERO,
            StatementData::Iteration(it) => self.scoped(|a| match &it.content {
                IterationStatementData::While(cond, body) => {
                    let mut i = a.condition(cond);
                    i.add(a.scoped(|a| a.statement(body)));
                    model::loop_cost(i)
                }
                IterationStatementData::DoWhile(body, cond) => {
                    let mut i = a.scoped(|a| a.statement(body));
                    i.add(a.expr(cond).0);
                    model::loop_cost(i)
                }
                IterationStatementData::For(init, rest, body) => {
                    let mut c = match &init.content {
                        ForInitStatementData::Expression(e) => {
                            e.as_ref().map_or(Cost::ZERO, |e| a.expr(e).0)
                        }
                        ForInitStatementData::Declaration(d) => a.local_declaration(d),
                    };
                    let mut i = rest
                        .condition
                        .as_ref()
                        .map_or(Cost::ZERO, |cond| a.condition(cond));
                    i.add(a.scoped(|a| a.statement(body)));
                    if let Some(p) = &rest.post_expr {
                        i.add(a.expr(p).0);
                    }
                    c.add(model::loop_cost(i));
                    c
                }
            }),
            StatementData::Jump(j) => match &j.content {
                JumpStatementData::Return(Some(e)) => self.expr(e).0,
                _ => Cost::ZERO,
            },
            StatementData::Compound(cs) => self.scoped(|a| {
                let mut c = Cost::ZERO;
                for s in &cs.statement_list {
                    c.add(a.statement(s));
                }
                c
            }),
        }
    }

    fn condition(&mut self, cond: &ConditionData) -> Cost {
        match cond {
            ConditionData::Expr(e) => self.expr(e).0,
            ConditionData::Assignment(ty, name, init) => {
                let t = self.type_spec(&ty.ty);
                let c = self.initializer(init);
                self.declare(name.0.to_string(), t);
                c
            }
        }
    }

    fn local_declaration(&mut self, d: &DeclarationData) -> Cost {
        let DeclarationData::InitDeclaratorList(l) = d else {
            return Cost::ZERO;
        };
        let head: &SingleDeclaration = &l.head;
        let base = self.type_spec(&head.ty.ty);
        let mut c = Cost::ZERO;
        if let Some(n) = &head.name {
            if let Some(i) = &head.initializer {
                c.add(self.initializer(i));
            }
            let t = self.with_array(base.clone(), head.array_specifier.as_ref());
            self.declare(n.0.to_string(), t);
        }
        for t in &l.tail {
            if let Some(i) = &t.initializer {
                c.add(self.initializer(i));
            }
            let ty = self.with_array(base.clone(), t.ident.array_spec.as_ref());
            self.declare(t.ident.ident.0.to_string(), ty);
        }
        c
    }

    fn initializer(&mut self, i: &InitializerData) -> Cost {
        match i {
            InitializerData::Simple(e) => self.expr(e).0,
            InitializerData::List(l) => {
                let mut c = Cost::ZERO;
                for i in l {
                    c.add(self.initializer(i));
                }
                c
            }
        }
    }

    // ---- expressions ----

    /// Result type of an arithmetic binary op and its ALU cost.
    fn arith(&self, op_is_mul: bool, l: &Ty, r: &Ty) -> (f64, Ty) {
        if op_is_mul
            && let (Ty::Num { rows: lr, cols: lc }, Ty::Num { rows: rr, cols: rc }) = (l, r)
        {
            let (lr, lc, rr, rc) = (*lr, *lc, *rr, *rc);
            let l_mat = lc > 1;
            let r_mat = rc > 1;
            if l_mat && r_mat {
                return (
                    model::mat_mat_cost(lr, rc, lc),
                    Ty::Num { rows: lr, cols: rc },
                );
            }
            if l_mat && rr > 1 {
                return (model::mat_vec_cost(lr, lc), Ty::vec(lr));
            }
            if r_mat && lr > 1 {
                return (model::mat_vec_cost(rr, rc), Ty::vec(rc));
            }
        }
        let t = if self.width(l) >= self.width(r) {
            l.clone()
        } else {
            r.clone()
        };
        (model::binary_cost(self.width(&t)), t)
    }

    fn expr(&mut self, e: &ExprData) -> (Cost, Ty) {
        match e {
            ExprData::Variable(id) => (Cost::ZERO, self.lookup(id.0.as_str())),
            ExprData::IntConst(_)
            | ExprData::UIntConst(_)
            | ExprData::BoolConst(_)
            | ExprData::FloatConst(_)
            | ExprData::DoubleConst(_) => (Cost::ZERO, SCALAR),
            ExprData::Unary(op, x) => {
                let (mut c, t) = self.expr(x);
                if !matches!(op.content, UnaryOpData::Add) {
                    c.alu += model::unary_cost(self.width(&t));
                }
                (c, t)
            }
            ExprData::Binary(op, l, r) => {
                let (mut c, lt) = self.expr(l);
                let (rc, rt) = self.expr(r);
                c.add(rc);
                use BinaryOpData as B;
                let (n, t) = match &op.content {
                    B::Or | B::Xor | B::And => (model::binary_cost(1), SCALAR),
                    B::Equal | B::NonEqual => (model::binary_cost(self.width(&lt)), SCALAR),
                    B::Lt | B::Gt | B::Lte | B::Gte => (model::binary_cost(1), SCALAR),
                    B::Mult => self.arith(true, &lt, &rt),
                    _ => self.arith(false, &lt, &rt),
                };
                c.alu += n;
                (c, t)
            }
            ExprData::Ternary(cond, a, b) => {
                let (mut c, _) = self.expr(cond);
                let (ac, at) = self.expr(a);
                let (bc, _) = self.expr(b);
                c.add(ac.max(bc));
                c.alu += model::select_cost(self.width(&at));
                (c, at)
            }
            ExprData::Assignment(lhs, op, rhs) => {
                let (mut c, lt) = self.expr(lhs);
                let (rc, rt) = self.expr(rhs);
                c.add(rc);
                c.alu += match &op.content {
                    AssignmentOpData::Equal => 0.0,
                    AssignmentOpData::Mult => self.arith(true, &lt, &rt).0,
                    _ => self.arith(false, &lt, &rt).0,
                };
                (c, lt)
            }
            ExprData::Bracket(base, idx) => {
                let (mut c, bt) = self.expr(base);
                c.add(self.expr(idx).0);
                let t = match bt {
                    Ty::Array(e, _) => *e,
                    Ty::Num { rows, cols } if cols > 1 => Ty::vec(rows),
                    Ty::Num { .. } => SCALAR,
                    _ => Ty::Other,
                };
                (c, t)
            }
            ExprData::Dot(base, field) => {
                let (c, bt) = self.expr(base);
                let name = field.0.as_str();
                let t = match &bt {
                    Ty::Struct(s) => self
                        .structs
                        .get(s)
                        .and_then(|f| f.iter().find(|(n, _)| n == name))
                        .map_or(Ty::Other, |(_, t)| t.clone()),
                    Ty::Num { cols: 1, .. } if is_swizzle(name) => Ty::vec(name.len() as u32),
                    _ => Ty::Other,
                };
                (c, t)
            }
            ExprData::PostInc(x) | ExprData::PostDec(x) => {
                let (mut c, t) = self.expr(x);
                c.alu += model::unary_cost(self.width(&t));
                (c, t)
            }
            ExprData::Comma(a, b) => {
                let (mut c, _) = self.expr(a);
                let (bc, bt) = self.expr(b);
                c.add(bc);
                (c, bt)
            }
            ExprData::FunCall(fid, args) => self.call(&fid.content, args),
        }
    }

    fn call(&mut self, fid: &FunIdentifierData, args: &[glsl_lang::ast::Expr]) -> (Cost, Ty) {
        let mut c = Cost::ZERO;
        let mut arg_tys = Vec::with_capacity(args.len());
        for a in args {
            let (ac, at) = self.expr(a);
            c.add(ac);
            arg_tys.push(at);
        }
        let name = match fid {
            // Constructors only move components.
            FunIdentifierData::TypeSpecifier(ts) => return (c, self.type_spec(ts)),
            FunIdentifierData::Expr(e) => match &e.content {
                ExprData::Variable(id) => id.0.to_string(),
                // `.length()` on arrays and similar method calls.
                _ => return (c, SCALAR),
            },
        };
        if self.structs.contains_key(&name) {
            return (c, Ty::Struct(name));
        }
        if let Some(defs) = self.functions.get(&name) {
            let fd = defs
                .iter()
                .find(|f| f.prototype.parameters.len() == args.len())
                .or(defs.first())
                .copied();
            if let Some(fd) = fd {
                c.add(self.call_def(fd));
                let rt = self.return_type(fd);
                return (c, rt);
            }
        }
        let first = arg_tys.first().cloned().unwrap_or(SCALAR);
        let w = self.width(&first);
        if name.starts_with("texture") || name.starts_with("texelFetch") {
            return match name.as_str() {
                "textureSize" => (c, Ty::Other),
                _ => {
                    c.add(Cost::sample());
                    (c, Ty::vec(4))
                }
            };
        }
        let lower = name.to_ascii_lowercase();
        let rt = match lower.as_str() {
            "dot" | "length" | "distance" | "determinant" | "any" | "all" => SCALAR,
            "cross" => Ty::vec(3),
            _ => first,
        };
        let cost_w = match lower.as_str() {
            "dot" | "length" | "distance" => w,
            _ => self.width(&rt),
        };
        c.alu += model::builtin_cost(&lower, cost_w);
        (c, rt)
    }
}

/// Types of GLSL built-in variables.
fn builtin_var(name: &str) -> Ty {
    match name {
        "gl_Position" | "gl_FragCoord" | "gl_FragColor" => Ty::vec(4),
        "gl_PointCoord" => Ty::vec(2),
        "gl_GlobalInvocationID"
        | "gl_LocalInvocationID"
        | "gl_WorkGroupID"
        | "gl_NumWorkGroups"
        | "gl_WorkGroupSize" => Ty::vec(3),
        "gl_VertexID"
        | "gl_InstanceID"
        | "gl_PointSize"
        | "gl_FragDepth"
        | "gl_FrontFacing"
        | "gl_LocalInvocationIndex" => SCALAR,
        _ => Ty::Other,
    }
}

fn is_swizzle(s: &str) -> bool {
    (1..=4).contains(&s.len())
        && (s.chars().all(|c| "xyzw".contains(c))
            || s.chars().all(|c| "rgba".contains(c))
            || s.chars().all(|c| "stpq".contains(c)))
}
