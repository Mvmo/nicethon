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
}
