use crate::ast::{self, Pos};
use crate::hir::*;
use std::collections::HashMap;

pub struct Diag {
    pub module: usize,
    pub pos: Pos,
    pub message: String,
}

#[derive(Clone, Debug)]
pub enum Binding {
    Def(DefId),
    Global(usize),
    TypedPy(usize, DefId),
    PyModule(usize, String),
    NiceModule(usize),
}

pub struct Source {
    pub module: ast::Module,
    pub imports: Vec<Option<usize>>,
}

#[derive(Default)]
pub struct FnState {
    pub locals: Vec<Local>,
    pub scopes: Vec<HashMap<String, usize>>,
    pub lambdas: Vec<Lambda>,
    pub owners: Vec<usize>,
    pub rets: Vec<Ty>,
    pub loops: Vec<Option<Ty>>,
    pub generics: Vec<String>,
    pub bounds: Vec<Vec<Bound>>,
    pub self_ty: Option<Ty>,
    pub class: Option<DefId>,
    pub is_async: bool,
    pub infer: Vec<(Ty, Pos, String)>,
}

#[derive(Clone, Default)]
pub struct FnContext {
    generics: Vec<String>,
    bounds: Vec<Vec<Bound>>,
    self_ty: Option<Ty>,
    class: Option<DefId>,
}

pub struct Checker {
    pub defs: Vec<Def>,
    pub globals: Vec<Global>,
    pub impls: Vec<Impl>,
    pub fns: Vec<FnBody>,
    pub subst: Vec<Option<Ty>>,
    pub diags: Vec<Diag>,
    pub scopes: Vec<HashMap<String, Binding>>,
    pub externs: HashMap<String, HashMap<String, DefId>>,
    pub builtins: HashMap<&'static str, DefId>,
    pub module: usize,
    pub f: FnState,
    bodies: Vec<(DefId, ast::FnDecl, FnContext)>,
    declared: Vec<Vec<Option<DefId>>>,
    defaults: Vec<(DefId, usize, ast::Expr)>,
}

pub const PRELUDE: usize = 0;

pub fn check(sources: &[Source], entry: usize) -> Result<Program, Vec<Diag>> {
    let mut checker = Checker {
        defs: vec![],
        globals: vec![],
        impls: vec![],
        fns: vec![],
        subst: vec![],
        diags: vec![],
        scopes: vec![HashMap::new(); sources.len()],
        externs: HashMap::new(),
        builtins: HashMap::new(),
        module: 0,
        f: FnState::default(),
        bodies: vec![],
        declared: vec![],
        defaults: vec![],
    };
    for (module, source) in sources.iter().enumerate() {
        checker.module = module;
        let declared = source.module.items.iter().map(|item| checker.declare(item)).collect();
        checker.declared.push(declared);
    }
    for name in ["Option", "Some", "None", "Result", "Ok", "Err", "PyErr", "Eq", "Ord", "Hash", "Show", "Iterator"] {
        if let Some(Binding::Def(def)) = checker.scopes[PRELUDE].get(name) {
            checker.builtins.insert(name, *def);
        }
    }
    for (module, source) in sources.iter().enumerate() {
        checker.module = module;
        for (import, target) in source.module.imports.iter().zip(&source.imports) {
            checker.import(import, *target);
        }
    }
    for (module, source) in sources.iter().enumerate() {
        checker.module = module;
        for (index, item) in source.module.items.iter().enumerate() {
            checker.signature(item, checker.declared[module][index]);
        }
    }
    checker.type_imports();
    checker.elaborate_defaults();
    checker.bodies();
    if checker.diags.is_empty() {
        Ok(Program { defs: checker.defs, fns: checker.fns, globals: checker.globals, impls: checker.impls, subst: checker.subst, entry_module: entry })
    } else {
        Err(checker.diags)
    }
}

impl Checker {
    pub fn error(&mut self, pos: Pos, message: impl Into<String>) {
        let message = message.into();
        if !self.diags.iter().any(|d| d.module == self.module && d.pos == pos) {
            self.diags.push(Diag { module: self.module, pos, message });
        }
    }

    fn add_def(&mut self, name: &str, export: bool, kind: DefKind) -> DefId {
        self.defs.push(Def { name: name.to_string(), module: self.module, export, kind });
        self.defs.len() - 1
    }

    pub fn add_global(&mut self, ty: Ty, init: GlobalInit, pos: Pos) -> usize {
        self.globals.push(Global { ty, init, module: self.module, pos });
        self.globals.len() - 1
    }

    fn bind(&mut self, name: &str, binding: Binding, pos: Pos) {
        if self.scopes[self.module].insert(name.to_string(), binding).is_some() {
            self.error(pos, format!("{name} is defined twice"));
        }
    }

    pub fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes[self.module].get(name).or_else(|| self.scopes[PRELUDE].get(name)).cloned()
    }

    pub fn empty_fn_state(&mut self) {
        self.f = FnState { owners: vec![0], scopes: vec![HashMap::new()], rets: vec![Ty::Unit], ..Default::default() };
    }

    // Declarations:

    fn declare(&mut self, item: &ast::Item) -> Option<DefId> {
        let pos = item.pos;
        let (name, kind) = match &item.kind {
            ast::ItemKind::Fn(decl) => {
                let def = self.add_def(&decl.name, item.export || decl.name == "main", DefKind::Placeholder);
                self.bind(&decl.name, Binding::Def(def), decl.pos);
                return Some(def);
            }
            ast::ItemKind::Enum(decl) => {
                let def = self.add_def(&decl.name, item.export, DefKind::Placeholder);
                self.bind(&decl.name, Binding::Def(def), pos);
                let generics = decl.generics.iter().map(|g| g.name.clone()).collect();
                let mut variants = vec![];
                for variant in &decl.variants {
                    let id = self.add_def(&variant.name, item.export, DefKind::Variant { parent: def, fields: vec![] });
                    variants.push(id);
                    if self.module == PRELUDE {
                        self.bind(&variant.name, Binding::Def(id), variant.pos);
                    }
                }
                self.defs[def].kind = DefKind::Enum { generics, variants };
                return Some(def);
            }
            ast::ItemKind::PyClass(decl) => {
                let def = self.add_def(&decl.name, item.export, DefKind::Placeholder);
                let global = self.add_global(Ty::Dyn, GlobalInit::Class(def), pos);
                self.defs[def].kind = DefKind::PyClass { global, fields: vec![], methods: vec![], bases: vec![] };
                self.bind(&decl.name, Binding::Def(def), pos);
                return Some(def);
            }
            ast::ItemKind::Extern(module, items) => {
                for extern_item in items {
                    let name = match extern_item {
                        ast::ExternItem::Fn(decl) => &decl.name,
                        ast::ExternItem::Const(name, _) => name,
                        ast::ExternItem::Type(decl) => &decl.name,
                    };
                    let def = self.add_def(name, false, DefKind::Placeholder);
                    self.externs.entry(module.clone()).or_default().insert(name.clone(), def);
                    if matches!(extern_item, ast::ExternItem::Type(_)) {
                        self.scopes[self.module].insert(name.clone(), Binding::Def(def));
                    }
                }
                return None;
            }
            ast::ItemKind::Impl(_) => return None,
            ast::ItemKind::Struct(decl) => (&decl.name, DefKind::Placeholder),
            ast::ItemKind::Trait(decl) => (&decl.name, DefKind::Trait { methods: vec![], generics: decl.generics.len() }),
            ast::ItemKind::TypeAlias(name, ..) | ast::ItemKind::Const(name, ..) => (name, DefKind::Placeholder),
        };
        let def = self.add_def(name, item.export, kind);
        self.bind(name, Binding::Def(def), pos);
        Some(def)
    }

    fn import(&mut self, import: &ast::Import, target: Option<usize>) {
        if import.py {
            let module_global = self.add_global(Ty::Dyn, GlobalInit::Import(import.from.clone()), import.pos);
            if let Some(alias) = &import.star {
                self.bind(alias, Binding::PyModule(module_global, import.from.clone()), import.pos);
            }
            for (name, alias, pos) in &import.names {
                let global = self.add_global(Ty::Dyn, GlobalInit::Attr(module_global, name.clone()), *pos);
                let binding = match self.externs.get(&import.from).and_then(|names| names.get(name)) {
                    Some(&def) => Binding::TypedPy(global, def),
                    None => Binding::Global(global),
                };
                self.bind(alias, binding, *pos);
            }
            return;
        }
        let Some(target) = target else { return };
        if let Some(alias) = &import.star {
            self.bind(alias, Binding::NiceModule(target), import.pos);
        }
        for (name, alias, pos) in &import.names {
            match self.scopes[target].get(name).cloned() {
                Some(Binding::Def(def)) if self.defs[def].export => self.bind(alias, Binding::Def(def), *pos),
                _ => self.error(*pos, format!("{} has no export {name}", import.from)),
            }
        }
    }

    fn type_imports(&mut self) {
        for module in 0..self.scopes.len() {
            let typed: Vec<(usize, DefId)> = self.scopes[module]
                .values()
                .filter_map(|b| match b {
                    Binding::TypedPy(global, def) => Some((*global, *def)),
                    _ => None,
                })
                .collect();
            for (global, def) in typed {
                self.globals[global].ty = match &self.defs[def].kind {
                    DefKind::Fn(sig) if sig.generics.is_empty() => Ty::Fn(sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(sig.ret.clone())),
                    DefKind::Alias(_, ty) => ty.clone(),
                    _ => Ty::Dyn,
                };
            }
        }
    }

    fn elaborate_defaults(&mut self) {
        for (def, index, expr) in std::mem::take(&mut self.defaults) {
            self.module = self.defs[def].module;
            let ty = match &self.defs[def].kind {
                DefKind::Fn(sig) => sig.params[index].ty.clone(),
                _ => self.fields_of(def)[index].ty.clone(),
            };
            self.empty_fn_state();
            let value = self.check_expr(&expr, Some(&ty));
            let value = self.coerce(value, &ty);
            match &mut self.defs[def].kind {
                DefKind::Fn(sig) => sig.params[index].default = Some(value),
                DefKind::Struct { fields, .. } | DefKind::PyClass { fields, .. } => fields[index].default = Some(value),
                _ => {}
            }
        }
    }

    // Signatures:

    fn signature(&mut self, item: &ast::Item, declared: Option<DefId>) {
        let def = declared.unwrap_or(0);
        match &item.kind {
            ast::ItemKind::Fn(decl) => {
                let ctx = self.generic_context(&decl.generics, vec![], vec![], None);
                let kind = if decl.body.is_some() { FnKind::Native } else { FnKind::Intrinsic };
                self.add_fn(Some(def), decl, ctx, None, kind, item);
            }
            ast::ItemKind::Struct(decl) => {
                let generics: Vec<String> = decl.generics.iter().map(|g| g.name.clone()).collect();
                let fields = self.fields(&decl.fields, &generics, def);
                self.defs[def].kind = DefKind::Struct { generics, fields };
            }
            ast::ItemKind::Enum(decl) => {
                let DefKind::Enum { generics, variants } = self.defs[def].kind.clone() else { unreachable!() };
                let ctx = FnContext { generics: generics.clone(), ..Default::default() };
                for (variant, id) in decl.variants.iter().zip(variants) {
                    let fields = variant.fields.iter().map(|(name, ty)| Field { name: name.clone(), ty: self.resolve_type(ty, &ctx), default: None }).collect();
                    self.defs[id].kind = DefKind::Variant { parent: def, fields };
                }
            }
            ast::ItemKind::Trait(decl) => {
                let mut generics = vec!["Self".to_string()];
                generics.extend(decl.generics.iter().map(|g| g.name.clone()));
                let mut bounds = vec![vec![]; generics.len()];
                bounds[0].push((def, (1..generics.len() as u32).map(Ty::Param).collect()));
                let mut methods = vec![];
                for method in &decl.methods {
                    let ctx = self.generic_context(&method.generics, generics.clone(), bounds.clone(), Some(Ty::Param(0)));
                    let kind = if method.body.is_some() { FnKind::Native } else { FnKind::TraitMethod };
                    let id = self.add_fn(None, method, ctx, Some(def), kind, item);
                    methods.push((method.name.clone(), id));
                }
                self.defs[def].kind = DefKind::Trait { methods, generics: decl.generics.len() };
            }
            ast::ItemKind::Impl(decl) => self.impl_block(decl),
            ast::ItemKind::Const(name, ty, value) => {
                let ty = self.resolve_type(ty, &FnContext::default());
                self.empty_fn_state();
                let value = self.check_expr(value, Some(&ty));
                let value = self.coerce(value, &ty);
                let global = self.add_global(ty.clone(), GlobalInit::Value(value), item.pos);
                self.scopes[self.module].insert(name.clone(), Binding::Global(global));
                self.defs[def].kind = DefKind::Alias(vec![], ty);
            }
            ast::ItemKind::TypeAlias(_, generics, ty) => {
                let names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
                let ctx = FnContext { generics: names.clone(), ..Default::default() };
                let ty = self.resolve_type(ty, &ctx);
                self.defs[def].kind = DefKind::Alias(names, ty);
            }
            ast::ItemKind::Extern(module, items) => {
                for extern_item in items {
                    match extern_item {
                        ast::ExternItem::Fn(decl) => {
                            let def = self.externs[module][&decl.name];
                            let ctx = self.generic_context(&decl.generics, vec![], vec![], None);
                            self.add_fn(Some(def), decl, ctx, None, FnKind::PyFunction, item);
                        }
                        ast::ExternItem::Const(name, ty) => {
                            let def = self.externs[module][name];
                            let ty = self.resolve_type(ty, &FnContext::default());
                            self.defs[def].kind = DefKind::Alias(vec![], ty);
                        }
                        ast::ExternItem::Type(decl) => self.extern_type(self.externs[module][&decl.name], decl, item),
                    }
                }
            }
            ast::ItemKind::PyClass(decl) => {
                let self_ty = Ty::Named(def, vec![]);
                let fields = self.fields(&decl.fields, &[], def);
                let mut methods = vec![];
                for method in &decl.methods {
                    let ast::ItemKind::Fn(fn_decl) = &method.kind else {
                        self.error(method.pos, "only methods and fields are allowed in a py class");
                        continue;
                    };
                    let mut ctx = self.generic_context(&fn_decl.generics, vec![], vec![], Some(self_ty.clone()));
                    ctx.class = Some(def);
                    let id = self.add_fn(None, fn_decl, ctx, Some(def), FnKind::Native, method);
                    methods.push((fn_decl.name.clone(), id));
                }
                self.empty_fn_state();
                let bases: Vec<Expr> = decl.bases.iter().map(|base| {
                    let base = self.check_expr(base, None);
                    self.dynamic(base)
                }).collect();
                if let DefKind::PyClass { fields: f, methods: m, bases: b, .. } = &mut self.defs[def].kind {
                    (*f, *m, *b) = (fields, methods, bases);
                }
            }
        }
    }

    fn fields(&mut self, fields: &[ast::Field], generics: &[String], def: DefId) -> Vec<Field> {
        let ctx = FnContext { generics: generics.to_vec(), ..Default::default() };
        let mut result = vec![];
        for (index, field) in fields.iter().enumerate() {
            if let Some(default) = &field.default {
                self.defaults.push((def, index, default.clone()));
            }
            result.push(Field { name: Some(field.name.clone()), ty: self.resolve_type(&field.ty, &ctx), default: None });
        }
        result
    }

    fn extern_type(&mut self, def: DefId, decl: &ast::ExternType, item: &ast::Item) {
        let generics: Vec<String> = decl.generics.iter().map(|g| g.name.clone()).collect();
        self.defs[def].kind = DefKind::ExternType { generics: generics.clone(), attrs: vec![], methods: vec![], bases: vec![] };
        let self_ty = Ty::Named(def, (0..generics.len() as u32).map(Ty::Param).collect());
        let ctx = FnContext { generics: generics.clone(), bounds: vec![vec![]; generics.len()], self_ty: Some(self_ty.clone()), class: None };
        let bases = decl.bases.iter().map(|b| self.resolve_type(b, &ctx)).collect();
        let attrs = decl.attrs.iter().map(|(n, t)| (n.clone(), self.resolve_type(t, &ctx))).collect();
        let mut methods = vec![];
        for method in &decl.methods {
            let ctx = self.generic_context(&method.generics, generics.clone(), vec![vec![]; generics.len()], Some(self_ty.clone()));
            methods.push((method.name.clone(), self.add_fn(None, method, ctx, Some(def), FnKind::PyMethod, item)));
        }
        self.defs[def].kind = DefKind::ExternType { generics, attrs, methods, bases };
    }

    fn add_fn(&mut self, def: Option<DefId>, decl: &ast::FnDecl, ctx: FnContext, owner: Option<DefId>, kind: FnKind, item: &ast::Item) -> DefId {
        let sig = self.fn_sig(decl, &ctx, owner, kind);
        let def = match def {
            Some(def) => {
                self.defs[def].kind = DefKind::Fn(sig);
                def
            }
            None => self.add_def(&decl.name, item.export, DefKind::Fn(sig)),
        };
        for (index, param) in decl.params.iter().enumerate() {
            if let Some(default) = &param.default {
                self.defaults.push((def, index, default.clone()));
            }
        }
        let generic = !self.sig(def).generics.is_empty();
        if generic && (item.export || !item.decorators.is_empty()) {
            self.error(decl.pos, "generic functions cannot be exported or decorated");
        }
        if decl.body.is_some() {
            self.bodies.push((def, decl.clone(), ctx));
        }
        if !item.decorators.is_empty() && matches!(item.kind, ast::ItemKind::Fn(_)) {
            self.empty_fn_state();
            let mut decorators = vec![];
            let mut ty = self.fn_type(def);
            for decorator in item.decorators.iter().rev() {
                let value = self.check_expr(decorator, None);
                ty = self.decorated_type(&value, ty, decorator.pos);
                decorators.push(value);
            }
            let global = self.add_global(ty, GlobalInit::Decorated(def, decorators), item.pos);
            self.scopes[self.module].insert(decl.name.clone(), Binding::Global(global));
        }
        def
    }

    fn decorated_type(&mut self, decorator: &Expr, input: Ty, pos: Pos) -> Ty {
        let ExprKind::Global(global) = decorator.kind else { return Ty::Dyn };
        let binding = self.scopes[self.module].values().find(|b| matches!(b, Binding::TypedPy(g, _) if *g == global)).cloned();
        let Some(Binding::TypedPy(_, def)) = binding else { return Ty::Dyn };
        let DefKind::Fn(sig) = self.defs[def].kind.clone() else { return Ty::Dyn };
        let (params, ret, _) = self.instantiate(&sig);
        if params.len() != 1 || !self.unify(&params[0], &input) {
            self.error(pos, "decorator does not accept this function");
        }
        self.deep(&ret)
    }

    fn generic_context(&mut self, generics: &[ast::Generic], outer: Vec<String>, outer_bounds: Vec<Vec<Bound>>, self_ty: Option<Ty>) -> FnContext {
        let mut ctx = FnContext { generics: outer, bounds: outer_bounds, self_ty, class: None };
        ctx.bounds.resize(ctx.generics.len(), vec![]);
        ctx.generics.extend(generics.iter().map(|g| g.name.clone()));
        // Bounds may name generics declared after them: <I: Iterator<T>, T>.
        let bounds: Vec<Vec<Bound>> = generics.iter().map(|g| g.bounds.iter().filter_map(|b| self.resolve_bound(b, &ctx)).collect()).collect();
        ctx.bounds.extend(bounds);
        ctx
    }

    fn resolve_bound(&mut self, ty: &ast::TypeExpr, ctx: &FnContext) -> Option<Bound> {
        if let ast::TypeExpr::Path(path, args, pos) = ty {
            if let Some(Binding::Def(def)) = self.lookup(&path[0]) {
                if matches!(self.defs[def].kind, DefKind::Trait { .. }) {
                    let args = args.iter().map(|arg| self.resolve_type(arg, ctx)).collect();
                    return Some((def, args));
                }
            }
            self.error(*pos, format!("{} is not a trait", path.join(".")));
        }
        None
    }

    fn fn_sig(&mut self, decl: &ast::FnDecl, ctx: &FnContext, owner: Option<DefId>, kind: FnKind) -> FnSig {
        let mut params = vec![];
        for param in &decl.params {
            let ty = match (&param.ty, param.kind) {
                (_, ast::ParamKind::SelfParam) => ctx.self_ty.clone().unwrap_or_else(|| {
                    self.error(param.pos, "self is only allowed in methods");
                    Ty::Error
                }),
                (Some(ty), _) => self.resolve_type(ty, ctx),
                (None, _) => Ty::Error,
            };
            let python = matches!(kind, FnKind::PyFunction | FnKind::PyMethod);
            if (param.optional || matches!(param.kind, ast::ParamKind::Star | ast::ParamKind::StarStar)) && !python {
                self.error(param.pos, "optional and spread parameters are only allowed in extern blocks");
            }
            params.push(Param { name: param.name.clone(), ty, default: None, optional: param.optional, kind: param.kind });
        }
        let ret = decl.ret.as_ref().map_or(Ty::Unit, |ty| self.resolve_type(ty, ctx));
        let generics = ctx.generics.iter().cloned().zip(ctx.bounds.iter().cloned()).collect();
        FnSig { generics, params, ret, is_async: decl.is_async, kind, owner }
    }

    fn impl_block(&mut self, decl: &ast::ImplDecl) {
        let names: Vec<String> = decl.generics.iter().map(|g| g.name.clone()).collect();
        let base = self.generic_context(&decl.generics, vec![], vec![], None);
        let target = self.resolve_type(&decl.target, &base);
        let (trait_def, trait_args) = match decl.trait_name.as_ref().and_then(|t| self.resolve_bound(t, &base)) {
            Some((def, args)) => (Some(def), args),
            None => (None, vec![]),
        };
        let mut methods = vec![];
        for item in &decl.methods {
            let ast::ItemKind::Fn(fn_decl) = &item.kind else {
                self.error(item.pos, "only methods are allowed in impl blocks");
                continue;
            };
            let ctx = self.generic_context(&fn_decl.generics, names.clone(), base.bounds.clone(), Some(target.clone()));
            let kind = if fn_decl.body.is_some() { FnKind::Native } else { FnKind::Intrinsic };
            methods.push((fn_decl.name.clone(), self.add_fn(None, fn_decl, ctx, None, kind, item)));
        }
        if let Some(trait_def) = trait_def {
            let DefKind::Trait { methods: required, .. } = self.defs[trait_def].kind.clone() else { unreachable!() };
            for (name, method) in required {
                if self.sig(method).kind == FnKind::TraitMethod && !methods.iter().any(|(m, _)| *m == name) {
                    self.error(decl.target.pos(), format!("missing trait method {name}"));
                }
            }
        }
        self.impls.push(Impl { trait_def, trait_args, target, generics: names.len(), methods });
    }

    pub fn resolve_type(&mut self, ty: &ast::TypeExpr, ctx: &FnContext) -> Ty {
        let (path, args, pos) = match ty {
            ast::TypeExpr::Tuple(items, _) if items.is_empty() => return Ty::Unit,
            ast::TypeExpr::Tuple(items, _) => return Ty::Tuple(items.iter().map(|t| self.resolve_type(t, ctx)).collect()),
            ast::TypeExpr::Fn(params, ret, _) => {
                let params = params.iter().map(|t| self.resolve_type(t, ctx)).collect();
                return Ty::Fn(params, Box::new(self.resolve_type(ret, ctx)));
            }
            ast::TypeExpr::Path(path, args, pos) => (path, args, *pos),
        };
        let args: Vec<Ty> = args.iter().map(|t| self.resolve_type(t, ctx)).collect();
        let name = path.join(".");
        let with_arity = |checker: &mut Self, expected: usize, make: &dyn Fn(&[Ty]) -> Ty| {
            if args.len() == expected {
                make(&args)
            } else {
                checker.error(pos, format!("{name} expects {expected} type arguments"));
                Ty::Error
            }
        };
        if path.len() == 1 {
            if let Some(index) = ctx.generics.iter().position(|g| *g == path[0]) {
                return Ty::Param(index as u32);
            }
            match path[0].as_str() {
                "Int" => return Ty::Int,
                "Float" => return Ty::Float,
                "Bool" => return Ty::Bool,
                "Str" => return Ty::Str,
                "Bytes" => return Ty::Bytes,
                "BigInt" => return Ty::BigInt,
                "Dyn" => return Ty::Dyn,
                "Never" => return Ty::Never,
                "Self" => return ctx.self_ty.clone().unwrap_or(Ty::Error),
                "List" => return with_arity(self, 1, &|a| Ty::List(Box::new(a[0].clone()))),
                "Set" => return with_arity(self, 1, &|a| Ty::Set(Box::new(a[0].clone()))),
                "Coroutine" => return with_arity(self, 1, &|a| Ty::Coroutine(Box::new(a[0].clone()))),
                "Map" => return with_arity(self, 2, &|a| Ty::Map(Box::new(a[0].clone()), Box::new(a[1].clone()))),
                _ => {}
            }
        }
        let def = match (self.lookup(&path[0]), path.len()) {
            (Some(Binding::Def(def) | Binding::TypedPy(_, def)), 1) => Some(def),
            (Some(Binding::NiceModule(module)), 2) => match self.scopes[module].get(&path[1]) {
                Some(Binding::Def(def)) if self.defs[*def].export => Some(*def),
                _ => None,
            },
            (Some(Binding::PyModule(_, module)), 2) => self.externs.get(&module).and_then(|names| names.get(&path[1]).copied()),
            _ => None,
        };
        let Some(def) = def else {
            self.error(pos, format!("unknown type {name}"));
            return Ty::Error;
        };
        match &self.defs[def].kind {
            DefKind::Struct { generics, .. } | DefKind::Enum { generics, .. } | DefKind::ExternType { generics, .. } => {
                let count = generics.len();
                with_arity(self, count, &|a| Ty::Named(def, a.to_vec()))
            }
            DefKind::PyClass { .. } | DefKind::Placeholder => Ty::Named(def, args),
            DefKind::Alias(generics, ty) => {
                let (count, ty) = (generics.len(), ty.clone());
                with_arity(self, count, &|a| ty.subst(a))
            }
            _ => {
                self.error(pos, format!("{name} is not a type"));
                Ty::Error
            }
        }
    }

    // Bodies:

    fn bodies(&mut self) {
        for (def, decl, ctx) in std::mem::take(&mut self.bodies) {
            self.module = self.defs[def].module;
            let sig = self.sig(def).clone();
            self.f = FnState {
                owners: vec![0],
                scopes: vec![HashMap::new()],
                rets: vec![sig.ret.clone()],
                generics: ctx.generics,
                bounds: ctx.bounds,
                self_ty: ctx.self_ty,
                class: ctx.class,
                is_async: sig.is_async,
                ..Default::default()
            };
            let params = decl.params.iter().zip(&sig.params).map(|(param, info)| self.declare_local(&param.name, info.ty.clone(), param.var)).collect();
            let body = self.check_block(decl.body.as_ref().unwrap(), Some(&sig.ret));
            let body = if sig.ret == Ty::Unit { body } else { self.coerce(body, &sig.ret) };
            for (ty, pos, name) in std::mem::take(&mut self.f.infer) {
                if matches!(self.deep(&ty), Ty::Var(_)) {
                    self.error(pos, format!("cannot infer type of lambda parameter {name}"));
                }
            }
            let state = std::mem::take(&mut self.f);
            self.fns.push(FnBody { def, params, locals: state.locals, lambdas: state.lambdas, body });
        }
    }

    // Types:

    pub fn sig(&self, def: DefId) -> &FnSig {
        match &self.defs[def].kind {
            DefKind::Fn(sig) => sig,
            _ => panic!("not a function: {}", self.defs[def].name),
        }
    }

    pub fn fields_of(&self, def: DefId) -> &[Field] {
        match &self.defs[def].kind {
            DefKind::Struct { fields, .. } | DefKind::Variant { fields, .. } | DefKind::PyClass { fields, .. } => fields,
            _ => &[],
        }
    }

    pub fn fn_type(&mut self, def: DefId) -> Ty {
        let sig = self.sig(def).clone();
        let (params, ret, _) = self.instantiate(&sig);
        let ret = if sig.is_async { Ty::Coroutine(Box::new(ret)) } else { ret };
        Ty::Fn(params, Box::new(ret))
    }

    pub fn fresh(&mut self) -> Ty {
        self.subst.push(None);
        Ty::Var(self.subst.len() as u32 - 1)
    }

    pub fn instantiate(&mut self, sig: &FnSig) -> (Vec<Ty>, Ty, Vec<Ty>) {
        let args: Vec<Ty> = sig.generics.iter().map(|_| self.fresh()).collect();
        let params = sig.params.iter().map(|p| p.ty.subst(&args)).collect();
        (params, sig.ret.subst(&args), args)
    }

    pub fn shallow(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(var) => match &self.subst[*var as usize] {
                Some(bound) => self.shallow(bound),
                None => ty.clone(),
            },
            _ => ty.clone(),
        }
    }

    pub fn deep(&self, ty: &Ty) -> Ty {
        ty.map(&mut |ty| match ty {
            Ty::Var(var) => self.subst[*var as usize].as_ref().map(|bound| self.deep(bound)),
            _ => None,
        })
    }

    pub fn unify(&mut self, left: &Ty, right: &Ty) -> bool {
        let (left, right) = (self.shallow(left), self.shallow(right));
        match (&left, &right) {
            (Ty::Error, _) | (_, Ty::Error) | (Ty::Never, _) | (_, Ty::Never) => true,
            (Ty::Var(a), Ty::Var(b)) if a == b => true,
            (Ty::Var(var), other) | (other, Ty::Var(var)) => {
                self.subst[*var as usize] = Some(other.clone());
                true
            }
            (Ty::List(a), Ty::List(b)) | (Ty::Set(a), Ty::Set(b)) | (Ty::Coroutine(a), Ty::Coroutine(b)) => self.unify(a, b),
            (Ty::Map(a, b), Ty::Map(c, d)) => self.unify(a, c) && self.unify(b, d),
            (Ty::Tuple(a), Ty::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| self.unify(x, y)),
            (Ty::Fn(a, r), Ty::Fn(b, s)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| self.unify(x, y)) && self.unify(r, s),
            (Ty::Named(a, x), Ty::Named(b, y)) => a == b && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.unify(p, q)),
            _ => left == right,
        }
    }

    pub fn show(&self, ty: &Ty) -> String {
        let list = |items: &[Ty]| items.iter().map(|t| self.show(t)).collect::<Vec<_>>().join(", ");
        match self.shallow(ty) {
            Ty::Int => "Int".into(),
            Ty::Float => "Float".into(),
            Ty::Bool => "Bool".into(),
            Ty::Str => "Str".into(),
            Ty::Bytes => "Bytes".into(),
            Ty::BigInt => "BigInt".into(),
            Ty::Unit => "()".into(),
            Ty::Never => "Never".into(),
            Ty::Dyn => "Dyn".into(),
            Ty::Error | Ty::Var(_) => "_".into(),
            Ty::List(item) => format!("List<{}>", self.show(&item)),
            Ty::Set(item) => format!("Set<{}>", self.show(&item)),
            Ty::Coroutine(item) => format!("Coroutine<{}>", self.show(&item)),
            Ty::Map(key, value) => format!("Map<{}, {}>", self.show(&key), self.show(&value)),
            Ty::Tuple(items) => format!("({})", list(&items)),
            Ty::Fn(params, ret) => format!("({}) -> {}", list(&params), self.show(&ret)),
            Ty::Named(def, args) if args.is_empty() => self.defs[def].name.clone(),
            Ty::Named(def, args) => format!("{}<{}>", self.defs[def].name, list(&args)),
            Ty::Param(index) => self.f.generics.get(index as usize).cloned().unwrap_or_else(|| format!("T{index}")),
        }
    }

    pub fn implements(&self, ty: &Ty, trait_def: DefId) -> bool {
        let ty = self.shallow(ty);
        let name = self.defs[trait_def].name.as_str();
        let builtin = self.builtins.get(name) == Some(&trait_def);
        let comparable = matches!(ty, Ty::Int | Ty::Float | Ty::Str | Ty::Bool | Ty::BigInt | Ty::Bytes);
        match &ty {
            Ty::Var(_) | Ty::Error | Ty::Never => true,
            Ty::Param(index) => self.f.bounds.get(*index as usize).is_some_and(|b| b.iter().any(|(t, _)| *t == trait_def)) || (builtin && name != "Ord"),
            _ if builtin && (name != "Ord" || comparable) => true,
            _ => self.impls.iter().any(|imp| imp.trait_def == Some(trait_def) && same_head(&imp.target, &ty)),
        }
    }
}

impl Checker {
    // The trait's type arguments for a type that implements it, e.g. [Int] for
    // ListIter<Int> and Iterator. None when the type does not implement it.
    pub fn trait_args_for(&mut self, ty: &Ty, trait_def: DefId) -> Option<Vec<Ty>> {
        let ty = self.shallow(ty);
        let count = match self.defs[trait_def].kind {
            DefKind::Trait { generics, .. } => generics,
            _ => 0,
        };
        match &ty {
            Ty::Var(_) | Ty::Error | Ty::Never => return Some((0..count).map(|_| self.fresh()).collect()),
            Ty::Param(index) => {
                let bound = self.f.bounds.get(*index as usize).and_then(|b| b.iter().find(|(t, _)| *t == trait_def));
                if let Some((_, args)) = bound {
                    return Some(args.clone());
                }
            }
            _ => {}
        }
        if count == 0 && self.implements(&ty, trait_def) {
            return Some(vec![]);
        }
        let imp = self.impls.iter().find(|imp| imp.trait_def == Some(trait_def) && same_head(&imp.target, &ty))?.clone();
        let fresh: Vec<Ty> = (0..imp.generics).map(|_| self.fresh()).collect();
        self.unify(&imp.target.subst(&fresh), &ty);
        Some(imp.trait_args.iter().map(|arg| arg.subst(&fresh)).collect())
    }
}

pub fn same_head(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Named(x, _), Ty::Named(y, _)) => x == y,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

impl Checker {
    pub fn resolve_here(&mut self, ty: &ast::TypeExpr) -> Ty {
        let ctx = FnContext { generics: self.f.generics.clone(), bounds: self.f.bounds.clone(), self_ty: self.f.self_ty.clone(), class: self.f.class };
        self.resolve_type(ty, &ctx)
    }

    pub fn builtin_ty(&self, name: &str, args: Vec<Ty>) -> Ty {
        Ty::Named(self.builtins[name], args)
    }
}
