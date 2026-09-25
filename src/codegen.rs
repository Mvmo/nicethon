use crate::ast::{BinOp, ParamKind, UnOp};
use crate::hir::*;
use std::collections::HashMap;
use std::fmt::Write;

pub fn generate(program: &Program, module: &str, names: &[String], texts: &[String]) -> String {
    let lines = texts.iter().map(|text| text.match_indices('\n').map(|(i, _)| i as u32).collect()).collect();
    let prelude = |name: &str| program.defs.iter().position(|d| d.module == 0 && d.name == name).unwrap();
    let parent = |variant: DefId| match program.defs[variant].kind {
        DefKind::Variant { parent, .. } => parent,
        _ => unreachable!(),
    };
    let (some, none, ok, err) = (prelude("Some"), prelude("None"), prelude("Ok"), prelude("Err"));
    let mut gen = Gen {
        p: program,
        module,
        names,
        lines,
        strings: vec![],
        string_index: HashMap::new(),
        kwnames: vec![],
        instances: vec![],
        instance_index: HashMap::new(),
        pending: vec![],
        bodies: program.fns.iter().enumerate().map(|(index, body)| (body.def, index)).collect(),
        records: vec![],
        record_index: HashMap::new(),
        enums: HashMap::new(),
        some,
        none,
        ok,
        err,
        option_type: parent(some),
        result_type: parent(ok),
        pyerr: prelude("PyErr"),
        code: String::new(),
        f: Func::default(),
    };
    gen.run()
}

#[derive(Clone, Default)]
enum Ret {
    #[default]
    Status,
    Native(Ty),
    Boxed(Ty),
}

enum Cleanup {
    Finally(Expr),
    WithExit(String),
}

struct LoopCtx {
    brk: String,
    cont: String,
    result: Option<(String, Ty)>,
    mark: usize,
    cleanups: usize,
}

#[derive(Default)]
struct Func {
    out: String,
    vars: Vec<(String, Repr)>,
    temps: usize,
    live: Vec<String>,
    created: Vec<String>,
    error: String,
    labels: usize,
    loops: Vec<LoopCtx>,
    cleanups: Vec<Cleanup>,
    body: Option<usize>,
    owner: usize,
    module: usize,
    ret: Ret,
    line: usize,
    awaits: usize,
    targs: Vec<Ty>,
    instance: usize,
}

struct Gen<'a> {
    p: &'a Program,
    module: &'a str,
    names: &'a [String],
    lines: Vec<Vec<u32>>,
    strings: Vec<String>,
    string_index: HashMap<String, usize>,
    kwnames: Vec<Vec<String>>,
    // One C function per (function, concrete type arguments).
    instances: Vec<(DefId, Vec<Ty>)>,
    instance_index: HashMap<(DefId, Vec<Ty>), usize>,
    pending: Vec<usize>,
    bodies: HashMap<DefId, usize>,
    // One C struct per (struct or variant, concrete type arguments).
    records: Vec<(DefId, Vec<Ty>)>,
    record_index: HashMap<(DefId, Vec<Ty>), usize>,
    enums: HashMap<(DefId, Vec<Ty>), usize>,
    some: DefId,
    none: DefId,
    ok: DefId,
    err: DefId,
    option_type: DefId,
    result_type: DefId,
    pyerr: DefId,
    code: String,
    f: Func,
}

fn c_type(repr: Repr) -> &'static str {
    match repr {
        Repr::I64 => "int64_t",
        Repr::F64 => "double",
        Repr::Bool | Repr::Unit => "int",
        Repr::Obj | Repr::OptObj => "PyObject *",
        Repr::OptI64 => "nt_OptI64",
        Repr::OptF64 => "nt_OptF64",
        Repr::OptBool => "nt_OptBool",
    }
}

fn zero(repr: Repr) -> &'static str {
    match repr {
        Repr::Obj | Repr::OptObj => "NULL",
        Repr::OptI64 => "((nt_OptI64){0, 0})",
        Repr::OptF64 => "((nt_OptF64){0, 0})",
        Repr::OptBool => "((nt_OptBool){0, 0})",
        _ => "0",
    }
}

fn error_value(repr: Repr) -> &'static str {
    match repr {
        Repr::Obj | Repr::OptObj => "NULL",
        Repr::F64 => "-1.0",
        Repr::OptI64 => "((nt_OptI64){0, -1})",
        Repr::OptF64 => "((nt_OptF64){0, -1})",
        Repr::OptBool => "((nt_OptBool){0, -1})",
        _ => "-1",
    }
}

fn kind(repr: Repr) -> &'static str {
    match repr {
        Repr::I64 => "NT_I64",
        Repr::F64 => "NT_F64",
        Repr::Bool => "NT_BOOL",
        _ => "NT_OBJ",
    }
}

fn c_string(text: &str) -> String {
    let mut out = String::from("\"");
    for byte in text.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            32..=126 => out.push(byte as char),
            _ => write!(out, "\\{byte:03o}").unwrap(),
        }
    }
    out.push('"');
    out
}

fn float_literal(value: f64) -> String {
    if value.is_nan() {
        "NAN".into()
    } else if value.is_infinite() {
        if value > 0.0 { "INFINITY".into() } else { "(-INFINITY)".into() }
    } else {
        format!("{value:?}")
    }
}

fn ident(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

impl<'a> Gen<'a> {
    // Output helpers:

    fn emit(&mut self, line: impl AsRef<str>) {
        self.f.out.push_str("    ");
        self.f.out.push_str(line.as_ref());
        self.f.out.push('\n');
    }

    fn temp(&mut self, repr: Repr) -> String {
        self.f.temps += 1;
        let name = format!("t{}", self.f.temps);
        self.f.vars.push((name.clone(), repr));
        if repr.owned() {
            self.f.live.push(name.clone());
            self.f.created.push(name.clone());
        }
        name
    }

    fn label(&mut self, prefix: &str) -> String {
        self.f.labels += 1;
        format!("{prefix}_{}", self.f.labels)
    }

    fn fail(&self) -> String {
        format!("{{ nt_lineno = {}; goto {}; }}", self.f.line, self.f.error)
    }

    fn check(&mut self, condition: &str) {
        let fail = self.fail();
        self.emit(format!("if ({condition}) {fail}"));
    }

    fn check_result(&mut self, value: &str, repr: Repr) {
        match repr {
            Repr::Obj => self.check(&format!("!{value}")),
            Repr::I64 => self.check(&format!("{value} == -1 && PyErr_Occurred()")),
            Repr::F64 => self.check(&format!("{value} == -1.0 && PyErr_Occurred()")),
            Repr::Bool | Repr::Unit => self.check(&format!("{value} < 0")),
            Repr::OptObj => self.check(&format!("!{value} && PyErr_Occurred()")),
            Repr::OptI64 | Repr::OptF64 | Repr::OptBool => self.check(&format!("{value}.tag < 0")),
        }
    }

    fn call(&mut self, repr: Repr, call: String) -> String {
        let result = self.temp(repr);
        self.emit(format!("{result} = {call};"));
        self.check_result(&result, repr);
        result
    }

    fn mark(&self) -> usize {
        self.f.live.len()
    }

    fn release(&mut self, mark: usize) {
        for name in self.f.live.split_off(mark.min(self.f.live.len())) {
            self.emit(format!("Py_CLEAR({name});"));
        }
    }

    fn release_copy(&mut self, mark: usize) {
        let clears: Vec<String> = self.f.live[mark.min(self.f.live.len())..].iter().map(|name| format!("Py_CLEAR({name});")).collect();
        if !clears.is_empty() {
            self.emit(clears.join(" "));
        }
    }

    fn string(&mut self, text: &str) -> String {
        let next = self.strings.len();
        let index = *self.string_index.entry(text.to_string()).or_insert(next);
        if index == next {
            self.strings.push(text.to_string());
        }
        format!("K{index}")
    }

    fn set_line(&mut self, pos: u32) {
        if let Some(lines) = self.lines.get(self.f.module) {
            self.f.line = lines.partition_point(|&end| end < pos) + 1;
        }
    }

    // Types:

    fn resolve(&self, ty: &Ty) -> Ty {
        self.p.resolve(ty).subst(&self.f.targs)
    }

    // Intrinsics are not instantiated, their type parameters stay boxed.
    fn erased(&self, ty: &Ty) -> Ty {
        self.p.resolve(ty).map(&mut |ty| matches!(ty, Ty::Param(_)).then_some(Ty::Dyn))
    }

    fn repr(&self, ty: &Ty) -> Repr {
        self.repr_of(&self.resolve(ty))
    }

    // Option<T> is unboxed: a {value, tag} struct for Int, Float and Bool, a
    // pointer that is NULL for None otherwise. Nested options stay records.
    fn repr_of(&self, ty: &Ty) -> Repr {
        match ty {
            Ty::Named(def, args) if *def == self.option_type => match self.repr_of(&args[0]) {
                Repr::I64 => Repr::OptI64,
                Repr::F64 => Repr::OptF64,
                Repr::Bool => Repr::OptBool,
                Repr::Obj => Repr::OptObj,
                _ => Repr::Obj,
            },
            _ => ty.repr(),
        }
    }

    // Record fields, list items, cells and globals keep options boxed.
    fn storage(&self, ty: &Ty) -> Repr {
        match self.repr(ty) {
            repr if repr.is_option() => Repr::Obj,
            repr => repr,
        }
    }

    fn show(&self, ty: &Ty) -> String {
        let list = |items: &[Ty]| items.iter().map(|t| self.show(t)).collect::<Vec<_>>().join(", ");
        match self.resolve(ty) {
            Ty::Int => "Int".into(),
            Ty::Float => "Float".into(),
            Ty::Bool => "Bool".into(),
            Ty::Str => "Str".into(),
            Ty::Bytes => "Bytes".into(),
            Ty::BigInt => "BigInt".into(),
            Ty::Unit => "()".into(),
            Ty::List(item) => format!("List<{}>", self.show(&item)),
            Ty::Set(item) => format!("Set<{}>", self.show(&item)),
            Ty::Map(key, value) => format!("Map<{}, {}>", self.show(&key), self.show(&value)),
            Ty::Tuple(items) => format!("({})", list(&items)),
            Ty::Fn(params, ret) => format!("({}) -> {}", list(&params), self.show(&ret)),
            Ty::Named(def, args) if args.is_empty() => self.p.defs[def].name.clone(),
            Ty::Named(def, args) => format!("{}<{}>", self.p.defs[def].name, list(&args)),
            _ => "Dyn".into(),
        }
    }

    fn spec(&mut self, ty: &Ty) -> String {
        match self.resolve(ty) {
            Ty::Int => "i".into(),
            Ty::Float => "f".into(),
            Ty::Bool => "b".into(),
            Ty::Str => "s".into(),
            Ty::Bytes => "y".into(),
            Ty::BigInt => "n".into(),
            Ty::Unit => "u".into(),
            Ty::List(item) => format!("L{}", self.spec(&item)),
            Ty::Set(item) => format!("S{}", self.spec(&item)),
            Ty::Map(key, value) => format!("M{}{}", self.spec(&key), self.spec(&value)),
            Ty::Tuple(items) => {
                let inner: String = items.iter().map(|t| self.spec(t)).collect();
                format!("T{};{inner}", items.len())
            }
            Ty::Fn(..) => "F".into(),
            Ty::Named(def, args) if def == self.option_type => {
                let (some, none) = (self.record(self.some, &args), self.record(self.none, &args));
                format!("O{some};{none};")
            }
            Ty::Named(def, args) => match &self.p.defs[def].kind {
                DefKind::Struct { .. } => format!("r{};", self.record(def, &args)),
                DefKind::Enum { .. } => format!("e{};", self.enum_id(def, &args)),
                _ => "d".into(),
            },
            _ => "d".into(),
        }
    }

    fn is_option(&self, ty: &Ty) -> bool {
        matches!(self.resolve(ty), Ty::Named(def, _) if def == self.option_type)
    }

    // Values are in the representation of the field types.
    fn make_record(&mut self, def: DefId, args: &[Ty], values: &[String]) -> String {
        let record = self.record(def, args);
        let p = self.p;
        let fields = p.fields(def);
        if fields.is_empty() {
            return format!("U{record}");
        }
        let stored: Vec<String> = values.iter().zip(fields).map(|(value, field)| self.stored(value, &field.ty.subst(args))).collect();
        let object = self.call(Repr::Obj, format!("nt_record_alloc((PyTypeObject *)&T{record})"));
        for (index, (value, field)) in stored.iter().zip(fields).enumerate() {
            let target = format!("((S{record} *){object})->f{index}");
            match self.storage(&field.ty.subst(args)) {
                Repr::Obj => self.emit(format!("{target} = Py_NewRef({value});")),
                Repr::Unit => self.emit(format!("{target} = Py_NewRef(Py_None);")),
                _ => self.emit(format!("{target} = {value};")),
            }
        }
        object
    }

    // Converts a value to its storage representation (boxes options).
    fn stored(&mut self, value: &str, ty: &Ty) -> String {
        if self.repr(ty).is_option() { self.box_value(value, ty) } else { value.to_string() }
    }

    // Boxing between representations:

    fn box_value(&mut self, value: &str, ty: &Ty) -> String {
        match self.repr(ty) {
            Repr::Obj => value.to_string(),
            Repr::Unit => "Py_None".into(),
            Repr::I64 => self.call(Repr::Obj, format!("PyLong_FromLongLong({value})")),
            Repr::F64 => self.call(Repr::Obj, format!("PyFloat_FromDouble({value})")),
            Repr::Bool => format!("({value} ? Py_True : Py_False)"),
            repr => {
                let args = self.owner_args(ty);
                let (present, payload) = if repr == Repr::OptObj { (value.to_string(), value.to_string()) } else { (format!("{value}.tag"), format!("{value}.value")) };
                let result = self.temp(Repr::Obj);
                self.emit(format!("if ({present}) {{"));
                let some = self.make_record(self.some, &args, &[payload]);
                self.emit(format!("{result} = Py_NewRef({some});"));
                self.emit("} else {");
                let none = self.make_record(self.none, &args, &[]);
                self.emit(format!("{result} = Py_NewRef({none});"));
                self.emit("}");
                result
            }
        }
    }

    fn unbox(&mut self, value: &str, repr: Repr) -> String {
        let some = |payload: &str, ctype: &str| format!("(NT_RECORD({value})->field_count ? ({ctype}){{((nt_{payload} *){value})->value, 1}} : ({ctype}){{0, 0}})");
        match repr {
            Repr::Obj => value.to_string(),
            Repr::Unit => "0".into(),
            Repr::I64 => self.call(Repr::I64, format!("PyLong_AsLongLong({value})")),
            Repr::F64 => self.call(Repr::F64, format!("PyFloat_AsDouble({value})")),
            Repr::Bool => format!("({value} == Py_True)"),
            Repr::OptObj => format!("(NT_RECORD({value})->field_count ? ((nt_Payload *){value})->value : NULL)"),
            Repr::OptI64 => some("SomeI64", "nt_OptI64"),
            Repr::OptF64 => some("SomeF64", "nt_OptF64"),
            Repr::OptBool => some("SomeBool", "nt_OptBool"),
        }
    }

    fn coerce(&mut self, value: &str, from: &Ty, to: &Ty) -> String {
        let (from_repr, to_repr) = (self.repr(from), self.repr(to));
        if matches!(self.resolve(from), Ty::Never) {
            return zero(to_repr).into();
        }
        if from_repr == to_repr {
            return value.to_string();
        }
        match (from_repr, to_repr) {
            (_, Repr::Unit) => "0".into(),
            (_, Repr::Obj) => self.box_value(value, from),
            (Repr::Obj, _) => self.unbox(value, to_repr),
            _ => value.to_string(),
        }
    }

    fn python_value(&mut self, value: &str, ty: &Ty) -> String {
        let boxed = self.box_value(value, ty);
        if self.is_option(ty) {
            return self.call(Repr::Obj, format!("nt_option_to_py({boxed})"));
        }
        boxed
    }

    fn checked_value(&mut self, value: &str, ty: &Ty, context: &str) -> String {
        let resolved = self.resolve(ty);
        if resolved == Ty::Dyn {
            return value.to_string();
        }
        let (spec, display) = (self.spec(&resolved), self.show(&resolved));
        let converted = self.call(Repr::Obj, format!("nt_from_py({value}, {}, {}, {})", c_string(&spec), c_string(&display), c_string(context)));
        let repr = self.repr_of(&resolved);
        self.unbox(&converted, repr)
    }

    // Locals:

    fn body(&self) -> &'a FnBody {
        &self.p.fns[self.f.body.unwrap()]
    }

    fn local_repr(&self, local: usize) -> Repr {
        let info = &self.body().locals[local];
        if info.cell { Repr::Obj } else { self.repr(&info.ty) }
    }

    fn declare_local(&mut self, local: usize) {
        let repr = self.local_repr(local);
        self.f.vars.push((format!("v{local}"), repr));
    }

    fn read_local(&mut self, local: usize) -> String {
        let info = &self.body().locals[local];
        let repr = self.repr(&info.ty);
        if info.cell {
            return self.unbox(&format!("PyCell_GET(v{local})"), repr);
        }
        if repr == Repr::Unit { "0".into() } else { format!("v{local}") }
    }

    fn write_local(&mut self, local: usize, value: &str, ty: &Ty, fresh: bool) {
        let info = &self.body().locals[local];
        let target = info.ty.clone();
        let value = self.coerce(value, ty, &target);
        if info.cell {
            let boxed = self.box_value(&value, &target);
            if fresh {
                let cell = self.call(Repr::Obj, format!("PyCell_New({boxed})"));
                self.emit(format!("Py_INCREF({cell}); Py_XSETREF(v{local}, {cell});"));
            } else {
                self.check(&format!("PyCell_Set(v{local}, {boxed}) < 0"));
            }
            return;
        }
        match self.repr(&target) {
            Repr::Obj => self.emit(format!("Py_INCREF({value}); Py_XSETREF(v{local}, {value});")),
            Repr::OptObj => self.emit(format!("Py_XINCREF({value}); Py_XSETREF(v{local}, {value});")),
            Repr::Unit => {}
            _ => self.emit(format!("v{local} = {value};")),
        }
    }

    // Functions:

    fn function(&mut self, header: String, trace: &str, func: Func, setup: impl FnOnce(&mut Self), value: Option<&Expr>) {
        let saved = std::mem::take(&mut self.f);
        self.f = Func { error: "error".into(), ..func };
        let (ret, module) = (self.f.ret.clone(), self.f.module);
        if let Some(index) = self.f.body {
            for (local, info) in self.p.fns[index].locals.iter().enumerate() {
                if info.owner == self.f.owner {
                    self.declare_local(local);
                }
            }
        }
        setup(self);
        if let Some(value) = value {
            let result = self.expr(value);
            let ty = value.ty.clone();
            if !matches!(self.resolve(&ty), Ty::Never) {
                self.emit_return(Some((result, ty)));
            }
        }
        let ret_repr = match &ret {
            Ret::Status => Repr::Unit,
            Ret::Native(ty) => self.repr(ty),
            Ret::Boxed(_) => Repr::Obj,
        };
        let mut text = format!("{header} {{\n    int nt_lineno = 0;\n    {} ret = {};\n", c_type(ret_repr), zero(ret_repr));
        let mut cleanup = String::new();
        for (name, repr) in &self.f.vars {
            writeln!(text, "    {} {name} = {};", c_type(*repr), zero(*repr)).unwrap();
            if repr.owned() {
                writeln!(cleanup, "    Py_XDECREF({name});").unwrap();
            }
        }
        text.push_str(&self.f.out);
        let file = &self.names[module];
        write!(text, "done:\n{cleanup}    return ret;\nerror:\n    nt_traceback({}, {}, nt_lineno);\n{cleanup}", c_string(trace), c_string(file)).unwrap();
        if ret_repr.owned() {
            text.push_str("    Py_XDECREF(ret);\n");
        }
        writeln!(text, "    return {};\n}}\n", error_value(ret_repr)).unwrap();
        self.code.push_str(&text);
        self.f = saved;
    }

    fn emit_return(&mut self, value: Option<(String, Ty)>) {
        match (self.f.ret.clone(), value) {
            (Ret::Native(declared), Some((value, ty))) => {
                let value = self.coerce(&value, &ty, &declared);
                match self.repr(&declared) {
                    Repr::Unit => {}
                    Repr::Obj => self.emit(format!("ret = {value}; Py_INCREF(ret);")),
                    Repr::OptObj => self.emit(format!("ret = {value}; Py_XINCREF(ret);")),
                    _ => self.emit(format!("ret = {value};")),
                }
            }
            (Ret::Boxed(_), value) => {
                let boxed = match value {
                    Some((value, ty)) => self.box_value(&value, &ty),
                    None => "Py_None".into(),
                };
                self.emit(format!("ret = {boxed}; Py_INCREF(ret);"));
            }
            _ => {}
        }
        self.run_cleanups(0);
        self.emit("goto done;");
    }

    fn run_cleanups(&mut self, depth: usize) {
        let cleanups: Vec<usize> = (depth..self.f.cleanups.len()).rev().collect();
        for index in cleanups {
            let saved = self.f.cleanups.split_off(index);
            match &saved[0] {
                Cleanup::Finally(block) => {
                    let block = block.clone();
                    self.stmt(&block);
                }
                Cleanup::WithExit(exit) => {
                    let exit = exit.clone();
                    self.call(Repr::Obj, format!("nt_with_exit({exit}, NULL)"));
                }
            }
            self.f.cleanups.extend(saved);
        }
    }

    fn symbol(&self, instance: usize) -> String {
        format!("F{instance}_{}", ident(&self.p.defs[self.instances[instance].0].name))
    }

    fn intrinsic_symbol(&self, def: DefId) -> String {
        let sig = self.p.sig(def);
        let owner = match sig.params.first().map(|p| (p.kind, self.erased(&p.ty))) {
            Some((ParamKind::SelfParam, Ty::List(_))) => "List_",
            Some((ParamKind::SelfParam, Ty::Str)) => "Str_",
            Some((ParamKind::SelfParam, Ty::Float)) => "Float_",
            Some((ParamKind::SelfParam, Ty::Int)) => "Int_",
            Some((ParamKind::SelfParam, Ty::Map(..))) => "Map_",
            _ => "",
        };
        format!("nt_{owner}{}", self.p.defs[def].name)
    }

    fn native_header(&self, instance: usize) -> String {
        let (def, targs) = &self.instances[instance];
        let sig = self.p.sig(*def);
        let concrete = |ty: &Ty| self.repr_of(&self.p.resolve(ty).subst(targs));
        let ret = if sig.is_async { Repr::Obj } else { concrete(&sig.ret) };
        let params: Vec<String> = sig.params.iter().enumerate().map(|(i, p)| format!("{} a{i}", c_type(concrete(&p.ty)))).collect();
        let params = if params.is_empty() { "void".into() } else { params.join(", ") };
        format!("static {} {}({params})", c_type(ret), self.symbol(instance))
    }

    fn native(&mut self, instance: usize) {
        let (def, targs) = self.instances[instance].clone();
        let index = self.bodies[&def];
        let p = self.p;
        let body = &p.fns[index];
        let sig = p.sig(def);
        let module = p.defs[def].module;
        let header = self.native_header(instance);
        let params: Vec<(usize, Ty)> = body.params.iter().zip(&sig.params).map(|(local, p)| (*local, p.ty.clone())).collect();
        let bind_params = |gen: &mut Self| {
            for (i, (local, ty)) in params.iter().enumerate() {
                gen.write_local(*local, &format!("a{i}"), ty, true);
            }
        };
        let func = Func { ret: Ret::Native(sig.ret.clone()), body: Some(index), module, targs, instance, ..Default::default() };
        if sig.is_async {
            self.coroutine(func, header, bind_params);
        } else {
            self.function(header, &p.defs[def].name, func, bind_params, Some(&body.body));
        }
        for lambda in 0..body.lambdas.len() {
            self.lambda(instance, lambda);
        }
        self.wrapper(instance);
    }

    // Every lambda becomes a native function N (captures, unboxed arguments)
    // and an entry L that Python calls with boxed arguments.
    fn lambda(&mut self, instance: usize, lambda: usize) {
        let (def, targs) = self.instances[instance].clone();
        let index = self.bodies[&def];
        let p = self.p;
        let info = &p.fns[index].lambdas[lambda];
        let module = p.defs[def].module;
        let (params, ret) = self.lambda_types(instance, lambda);
        let native = format!("N{instance}_{lambda}");
        let setup = |gen: &mut Self| {
            for &capture in &info.captures {
                gen.declare_local(capture);
            }
            for (position, &capture) in info.captures.iter().enumerate() {
                let item = format!("PyTuple_GET_ITEM(self, {position})");
                if gen.body().locals[capture].cell {
                    gen.emit(format!("v{capture} = Py_NewRef({item});"));
                } else {
                    let ty = gen.body().locals[capture].ty.clone();
                    let repr = gen.repr(&ty);
                    let value = gen.unbox(&item, repr);
                    gen.write_local(capture, &value, &ty, true);
                }
            }
            for (position, &param) in info.params.iter().enumerate() {
                let ty = gen.body().locals[param].ty.clone();
                gen.write_local(param, &format!("a{position}"), &ty, true);
            }
        };
        let func = Func { ret: Ret::Native(info.ret.clone()), body: Some(index), owner: lambda + 1, module, targs: targs.clone(), instance, ..Default::default() };
        self.function(self.native_signature(&native, &params, &ret), "<lambda>", func, setup, Some(&info.body));
        let names: Vec<String> = info.params.iter().map(|&param| p.fns[index].locals[param].name.clone()).collect();
        let header = format!("static PyObject *L{instance}_{lambda}(PyObject *self, PyObject *const *args, Py_ssize_t nargs, PyObject *kwnames)");
        let entry = |gen: &mut Self| {
            let count = params.len();
            let fail = gen.fail();
            gen.emit(format!("if (nargs != {count} || kwnames) {{ PyErr_SetString(PyExc_TypeError, \"lambda takes {count} positional arguments\"); {fail} }}"));
            let values: Vec<String> = params.iter().zip(&names).enumerate().map(|(i, (ty, name))| gen.checked_value(&format!("args[{i}]"), ty, name)).collect();
            let arguments: String = values.iter().map(|v| format!(", {v}")).collect();
            let result = gen.call(gen.repr_of(&ret), format!("{native}(self{arguments})"));
            let boxed = gen.box_value(&result, &ret);
            gen.emit(format!("ret = {boxed}; Py_INCREF(ret);"));
        };
        self.function(header, "<lambda>", Func { ret: Ret::Boxed(Ty::Dyn), module, targs, instance, ..Default::default() }, entry, None);
    }

    fn wrapper(&mut self, instance: usize) {
        let (def, targs) = self.instances[instance].clone();
        let p = self.p;
        let sig = p.sig(def);
        let name = p.defs[def].name.clone();
        let count = sig.params.len();
        let names: Vec<String> = sig.params.iter().map(|p| c_string(&p.name)).collect();
        let header = format!("static PyObject *W{instance}(PyObject *self, PyObject *const *args, Py_ssize_t nargs, PyObject *kwnames)");
        let module = p.defs[def].module;
        let ret_ty = if sig.is_async { Ty::Dyn } else { sig.ret.clone() };
        let setup = |gen: &mut Self| {
            gen.emit(format!("static const char *const names[] = {{{}NULL}};", names.iter().map(|n| format!("{n}, ")).collect::<String>()));
            gen.emit(format!("PyObject *slots[{}] = {{0}};", count + 1));
            let fail = gen.fail();
            gen.emit(format!("if (nt_args(args, nargs, kwnames, names, {count}, slots) < 0) {fail}"));
            let mut values = vec![];
            for (i, param) in sig.params.iter().enumerate() {
                let repr = gen.repr(&param.ty);
                let slot = gen.temp(repr);
                match &param.default {
                    Some(default) => {
                        gen.emit(format!("if (!slots[{i}]) {{"));
                        let value = gen.expr(default);
                        let value = gen.coerce(&value, &default.ty, &param.ty);
                        gen.assign(&slot, &value, repr);
                        gen.emit("} else {");
                    }
                    None => {
                        let missing = format!("missing argument '{}'", param.name);
                        gen.emit(format!("if (!slots[{i}]) {{ PyErr_SetString(PyExc_TypeError, {}); {fail} }}", c_string(&missing)));
                        gen.emit("{");
                    }
                }
                let value = gen.checked_value(&format!("slots[{i}]"), &param.ty, &param.name);
                gen.assign(&slot, &value, repr);
                gen.emit("}");
                values.push(slot);
            }
            let ret_repr = if sig.is_async { Repr::Obj } else { gen.repr(&ret_ty) };
            let result = gen.call(ret_repr, format!("{}({})", gen.symbol(instance), values.join(", ")));
            let boxed = gen.python_value(&result, &ret_ty);
            gen.emit(format!("ret = {boxed}; Py_INCREF(ret);"));
        };
        self.function(header, &name, Func { ret: Ret::Boxed(Ty::Dyn), module, targs, ..Default::default() }, setup, None);
        let arguments: Vec<String> = (0..count).map(|i| format!("a{i}")).collect();
        writeln!(self.code, "{} {{\n    return {}({});\n}}\n", self.adapter_header(instance), self.symbol(instance), arguments.join(", ")).unwrap();
    }

    // NF adapts a named function to the closure calling convention.
    fn adapter_header(&self, instance: usize) -> String {
        let (def, targs) = &self.instances[instance];
        let sig = self.p.sig(*def);
        let concrete = |ty: &Ty| self.p.resolve(ty).subst(targs);
        let params: Vec<Ty> = sig.params.iter().map(|p| concrete(&p.ty)).collect();
        let ret = if sig.is_async { Ty::Dyn } else { concrete(&sig.ret) };
        self.native_signature(&format!("NF{instance}"), &params, &ret)
    }

    fn call_function(&mut self, def: DefId, targs: &[Ty], args: &[Expr], ty: &Ty) -> String {
        let p = self.p;
        let sig = p.sig(def);
        let raw: Vec<String> = args.iter().map(|arg| self.expr(arg)).collect();
        if sig.kind == FnKind::Intrinsic {
            return self.call_intrinsic(def, args, &raw, ty);
        }
        let targs: Vec<Ty> = targs.iter().map(|t| self.resolve(t)).collect();
        let instance = self.instance(def, &targs);
        let values: Vec<String> = raw.iter().zip(args).zip(&sig.params).map(|((value, arg), param)| self.coerce(value, &arg.ty, &param.ty.subst(&targs))).collect();
        let call = format!("{}({})", self.symbol(instance), values.join(", "));
        if sig.is_async {
            return self.call(Repr::Obj, call);
        }
        let ret = self.p.resolve(&sig.ret).subst(&targs);
        let result = self.call(self.repr_of(&ret), call);
        self.coerce(&result, &ret, ty)
    }

    fn call_intrinsic(&mut self, def: DefId, args: &[Expr], raw: &[String], ty: &Ty) -> String {
        let sig = self.p.sig(def);
        let symbol = self.intrinsic_symbol(def);
        let params: Vec<Ty> = sig.params.iter().map(|p| self.erased(&p.ty)).collect();
        // Pushing an unboxed Int or Float skips the boxed path.
        let call = match (symbol.as_str(), args.get(1).map(|arg| self.repr(&arg.ty))) {
            ("nt_List_push", Some(Repr::I64)) => format!("nt_list_append_i64({}, {})", raw[0], raw[1]),
            ("nt_List_push", Some(Repr::F64)) => format!("nt_list_append_f64({}, {})", raw[0], raw[1]),
            _ => {
                let values: Vec<String> = raw.iter().zip(args).zip(&params).map(|((value, arg), param)| self.coerce(value, &arg.ty, param)).collect();
                format!("{symbol}({})", values.join(", "))
            }
        };
        if self.is_option(&sig.ret) {
            let payload = self.temp(Repr::Obj);
            self.emit(format!("{payload} = {call};"));
            self.check(&format!("!{payload} && PyErr_Occurred()"));
            return self.make_option(&payload, ty);
        }
        let declared = self.erased(&sig.ret);
        let result = self.call(self.repr_of(&declared), call);
        self.coerce(&result, &declared, ty)
    }

    fn assign(&mut self, target: &str, value: &str, repr: Repr) {
        match repr {
            Repr::Obj => self.emit(format!("{target} = Py_NewRef({value});")),
            Repr::OptObj => self.emit(format!("{target} = Py_XNewRef({value});")),
            Repr::Unit => {}
            _ => self.emit(format!("{target} = {value};")),
        }
    }

    // Statements:

    fn stmt(&mut self, expr: &Expr) {
        let mark = self.mark();
        self.expr(expr);
        self.release(mark);
    }

    fn block_stmt(&mut self, stmt: &Stmt) {
        let mark = self.mark();
        match stmt {
            Stmt::Expr(expr) => {
                self.expr(expr);
            }
            Stmt::Let(pattern, value) => {
                let result = self.expr(value);
                if !matches!(self.resolve(&value.ty), Ty::Never) {
                    self.pattern(pattern, &result, &mut |_| {});
                }
            }
        }
        self.release(mark);
    }

    fn cond(&mut self, expr: &Expr) -> String {
        let value = self.expr(expr);
        let flag = self.temp(Repr::Bool);
        self.emit(format!("{flag} = {value};"));
        flag
    }

    // Expressions:

    fn expr(&mut self, e: &Expr) -> String {
        self.set_line(e.pos);
        let ty = self.resolve(&e.ty);
        let repr = self.repr_of(&ty);
        match &e.kind {
            ExprKind::Int(value) if ty == Ty::BigInt => self.call(Repr::Obj, format!("PyLong_FromLongLong(INT64_C({value}))")),
            ExprKind::Int(value) => format!("INT64_C({value})"),
            ExprKind::Float(value) => float_literal(*value),
            ExprKind::Bool(value) => (*value as i32).to_string(),
            ExprKind::Str(text) => self.string(text),
            ExprKind::Bytes(bytes) => {
                let literal: String = bytes.iter().map(|b| format!("\\{b:03o}")).collect();
                self.call(Repr::Obj, format!("PyBytes_FromStringAndSize(\"{literal}\", {})", bytes.len()))
            }
            ExprKind::Unit => "0".into(),
            ExprKind::Local(local) => self.read_local(*local),
            ExprKind::Global(global) => match self.repr(&self.p.globals[*global].ty) {
                Repr::Unit => "0".into(),
                repr if repr.is_option() => self.unbox(&format!("G{global}"), repr),
                _ => format!("G{global}"),
            },
            ExprKind::FnRef(def, targs) => {
                let targs: Vec<Ty> = targs.iter().map(|t| self.resolve(t)).collect();
                format!("V{}", self.instance(*def, &targs))
            }
            ExprKind::Lambda(lambda) => {
                let instance = self.f.instance;
                let captures = self.body().lambdas[*lambda].captures.clone();
                let tuple = self.call(Repr::Obj, format!("PyTuple_New({})", captures.len()));
                for (position, capture) in captures.iter().enumerate() {
                    let item = if self.body().locals[*capture].cell {
                        format!("v{capture}")
                    } else {
                        let ty = self.body().locals[*capture].ty.clone();
                        let value = self.read_local(*capture);
                        self.box_value(&value, &ty)
                    };
                    self.emit(format!("PyTuple_SET_ITEM({tuple}, {position}, Py_NewRef({item}));"));
                }
                self.call(Repr::Obj, format!("nt_closure_new(L{instance}_{lambda}, (void *)N{instance}_{lambda}, {tuple}, \"<lambda>\")"))
            }
            ExprKind::Call(def, targs, args) => self.call_function(*def, targs, args, &ty),
            ExprKind::CallValue(function, args) => {
                let Ty::Fn(params, ret) = self.resolve(&function.ty) else { unreachable!() };
                let function = self.expr(function);
                let values: Vec<String> = args.iter().zip(&params).map(|(arg, param)| {
                    let value = self.expr(arg);
                    self.coerce(&value, &arg.ty, param)
                }).collect();
                let ret_repr = self.repr(&ret);
                let result = self.temp(ret_repr);
                let pointer: String = params.iter().map(|p| format!(", {}", c_type(self.repr(p)))).collect();
                let arguments: String = values.iter().map(|v| format!(", {v}")).collect();
                // Nicethon closures are called directly, other callables through Python.
                self.emit(format!("if (NT_NATIVE({function})) {{"));
                self.emit(format!("{result} = (({} (*)(PyObject *{pointer}))NT_NATIVE({function}))(((nt_Closure *){function})->captures{arguments});", c_type(ret_repr)));
                self.check_result(&result, ret_repr);
                self.emit("} else {");
                let boxed: Vec<String> = values.iter().zip(&params).map(|(value, param)| self.box_value(value, param)).collect();
                let slow = self.vectorcall(&function, &boxed, None);
                let converted = self.checked_value(&slow, &ret, "");
                self.assign(&result, &converted, ret_repr);
                self.emit("}");
                self.coerce(&result, &ret, &ty)
            }
            ExprKind::PyCall(function, args) => {
                let function = self.expr(function);
                self.py_call(&function, args)
            }
            ExprKind::PyAttr(object, name) => {
                let object = self.expr(object);
                let name = self.string(name);
                self.call(Repr::Obj, format!("PyObject_GetAttr({object}, {name})"))
            }
            ExprKind::PyIndex(object, parts) => {
                let object = self.expr(object);
                let key = self.index_key(parts);
                self.call(Repr::Obj, format!("PyObject_GetItem({object}, {key})"))
            }
            ExprKind::TraitCall(method, targs, args) => {
                let (def, targs) = self.trait_target(*method, targs);
                self.call_function(def, &targs, args, &ty)
            }
            ExprKind::Super(global, this) => {
                let this = self.read_local(*this);
                self.call(Repr::Obj, format!("PyObject_CallFunctionObjArgs((PyObject *)&PySuper_Type, G{global}, {this}, NULL)"))
            }
            ExprKind::ToDyn(inner) => {
                let value = self.expr(inner);
                self.python_value(&value, &inner.ty)
            }
            ExprKind::FromDyn(inner, optional) => {
                let value = self.expr(inner);
                if !optional {
                    return self.checked_value(&value, &ty, "");
                }
                let spec = self.spec(&self.owner_args(&ty)[0]);
                let converted = self.temp(Repr::Obj);
                self.emit(format!("{converted} = nt_try_convert({value}, {});", c_string(&spec)));
                self.make_option(&converted, &ty)
            }
            ExprKind::Construct(def, args) if repr.is_option() => {
                let payload = args.first().map(|arg| {
                    let value = self.expr(arg);
                    self.coerce(&value, &arg.ty, &self.owner_args(&ty)[0])
                });
                let _ = def;
                self.option_value(&ty, payload.as_deref())
            }
            ExprKind::Construct(def, args) => {
                let owner_args = self.owner_args(&ty);
                let p = self.p;
                let values: Vec<String> = args.iter().zip(p.fields(*def)).map(|(arg, field)| {
                    let value = self.expr(arg);
                    self.coerce(&value, &arg.ty, &field.ty.subst(&owner_args))
                }).collect();
                self.make_record(*def, &owner_args, &values)
            }
            ExprKind::Field(object, def, index) => {
                let value = self.expr(object);
                self.field(&value, *def, *index, &object.ty, &ty)
            }
            ExprKind::CopyWith(object, def, updates) => {
                let args = self.owner_args(&object.ty);
                let record = self.record(*def, &args);
                let object = self.expr(object);
                let p = self.p;
                let values: Vec<(usize, String, Repr)> = updates.iter().map(|(index, value)| {
                    let field_ty = p.fields(*def)[*index].ty.subst(&args);
                    let result = self.expr(value);
                    let converted = self.coerce(&result, &value.ty, &field_ty);
                    (*index, self.stored(&converted, &field_ty), self.storage(&field_ty))
                }).collect();
                let copy = self.call(Repr::Obj, format!("nt_record_copy({object})"));
                for (index, value, repr) in values {
                    match repr {
                        Repr::Obj => self.emit(format!("Py_SETREF(((S{record} *){copy})->f{index}, Py_NewRef({value}));")),
                        Repr::Unit => {}
                        _ => self.emit(format!("((S{record} *){copy})->f{index} = {value};")),
                    }
                }
                copy
            }
            ExprKind::TupleGet(object, index) => {
                let object = self.expr(object);
                self.unbox(&format!("PyTuple_GET_ITEM({object}, {index})"), repr)
            }
            ExprKind::Tuple(items) => {
                let values: Vec<String> = items.iter().map(|item| {
                    let value = self.expr(item);
                    self.box_value(&value, &item.ty)
                }).collect();
                let tuple = self.call(Repr::Obj, format!("PyTuple_New({})", values.len()));
                for (index, value) in values.iter().enumerate() {
                    self.emit(format!("PyTuple_SET_ITEM({tuple}, {index}, Py_NewRef({value}));"));
                }
                tuple
            }
            ExprKind::List(items) => {
                let Ty::List(item_ty) = &ty else { unreachable!() };
                let item_repr = self.repr(item_ty);
                let values: Vec<String> = items.iter().map(|item| self.expr(item)).collect();
                let list = self.call(Repr::Obj, format!("nt_list_new({}, {})", kind(item_repr), values.len()));
                match self.resolve(item_ty) {
                    Ty::Str => self.emit(format!("((nt_List *){list})->item_type = &PyUnicode_Type;")),
                    Ty::Named(def, args) if matches!(self.p.defs[def].kind, DefKind::Struct { .. }) => {
                        let record = self.record(def, &args);
                        self.emit(format!("((nt_List *){list})->item_type = (PyTypeObject *)&T{record};"))
                    }
                    _ => {}
                }
                for value in values {
                    let append = match item_repr {
                        Repr::I64 => "nt_list_append_i64",
                        Repr::F64 => "nt_list_append_f64",
                        Repr::Bool => "nt_list_append_bool",
                        _ => "nt_list_append",
                    };
                    let value = if item_repr == Repr::Unit { "Py_None".to_string() } else { self.stored(&value, item_ty) };
                    self.check(&format!("{append}({list}, {value}) < 0"));
                }
                list
            }
            ExprKind::Map(entries) => {
                let map = self.call(Repr::Obj, "PyDict_New()".into());
                for (key, value) in entries {
                    let k = self.expr(key);
                    let k = self.box_value(&k, &key.ty);
                    let v = self.expr(value);
                    let v = self.box_value(&v, &value.ty);
                    self.check(&format!("PyDict_SetItem({map}, {k}, {v}) < 0"));
                }
                map
            }
            ExprKind::Range(start, end, inclusive) => {
                let start = self.expr(start);
                let end = self.expr(end);
                self.call(Repr::Obj, format!("nt_range({start}, {end}{})", if *inclusive { " + 1" } else { "" }))
            }
            ExprKind::Binary(op, left, right) => self.binary(*op, left, right, &ty),
            ExprKind::Inplace(op, target, value) => {
                let target = self.expr(target);
                let value = self.expr(value);
                let function = py_number(*op).replace("PyNumber_", "PyNumber_InPlace");
                let extra = if *op == BinOp::Pow { ", Py_None" } else { "" };
                self.call(Repr::Obj, format!("{function}({target}, {value}{extra})"))
            }
            ExprKind::Unary(op, inner) => {
                let inner_ty = self.resolve(&inner.ty);
                let value = self.expr(inner);
                match (op, inner_ty.repr()) {
                    (UnOp::Neg, Repr::I64) => {
                        self.emit(format!("if ({value} == INT64_MIN) {{ nt_overflow(); {} }}", self.fail()));
                        format!("(-{value})")
                    }
                    (UnOp::Neg, Repr::F64) => format!("(-{value})"),
                    (UnOp::Neg, _) => self.call(Repr::Obj, format!("PyNumber_Negative({value})")),
                    (UnOp::Not, Repr::Bool) => format!("(!{value})"),
                    (UnOp::Not, _) => {
                        let flag = self.call(Repr::Bool, format!("PyObject_Not({value})"));
                        format!("({flag} ? Py_True : Py_False)")
                    }
                    (UnOp::BitNot, Repr::I64) => format!("(~{value})"),
                    (UnOp::BitNot, _) => self.call(Repr::Obj, format!("PyNumber_Invert({value})")),
                }
            }
            ExprKind::Cast(inner) => {
                let from = self.resolve(&inner.ty);
                let value = self.expr(inner);
                match (from, &ty) {
                    (Ty::Int, Ty::Float) => format!("((double){value})"),
                    (Ty::Float, Ty::Int) => {
                        let result = self.temp(Repr::I64);
                        self.check(&format!("nt_float_to_int({value}, &{result}) < 0"));
                        result
                    }
                    (Ty::Int, Ty::BigInt) => self.call(Repr::Obj, format!("PyLong_FromLongLong({value})")),
                    (Ty::BigInt, Ty::Int) => self.call(Repr::I64, format!("nt_big_to_i64({value})")),
                    (Ty::BigInt, Ty::Float) => self.call(Repr::F64, format!("PyLong_AsDouble({value})")),
                    _ => value,
                }
            }
            ExprKind::Index(object, index) => {
                let object_ty = self.resolve(&object.ty);
                let object = self.expr(object);
                let index_value = self.expr(index);
                match object_ty {
                    Ty::Map(..) => {
                        let key = self.box_value(&index_value, &index.ty);
                        let value = self.call(Repr::Obj, format!("nt_map_get({object}, {key})"));
                        self.unbox(&value, repr)
                    }
                    _ => self.list_get(&object, &index_value, &ty),
                }
            }
            ExprKind::Template(parts) => {
                let values: Vec<String> = parts.iter().map(|part| {
                    let value = self.expr(part);
                    self.box_value(&value, &part.ty)
                }).collect();
                let argv = self.argv(&values);
                self.call(Repr::Obj, format!("nt_template({}, {argv})", values.len()))
            }
            ExprKind::Block(stmts, tail) => {
                for stmt in stmts {
                    self.block_stmt(stmt);
                }
                match tail {
                    Some(tail) => {
                        let value = self.expr(tail);
                        self.coerce(&value, &tail.ty, &ty)
                    }
                    None => zero(repr).into(),
                }
            }
            ExprKind::If(cond, then, otherwise) => {
                let flag = self.cond(cond);
                let result = (repr != Repr::Unit && otherwise.is_some()).then(|| self.temp(repr));
                self.emit(format!("if ({flag}) {{"));
                self.branch(then, result.as_deref(), &ty);
                if let Some(otherwise) = otherwise {
                    self.emit("} else {");
                    self.branch(otherwise, result.as_deref(), &ty);
                }
                self.emit("}");
                result.unwrap_or_else(|| zero(repr).into())
            }
            ExprKind::Match(scrutinee, arms) => {
                let value = self.expr(scrutinee);
                let result = (repr != Repr::Unit).then(|| self.temp(repr));
                let end = self.label("match_end");
                for arm in arms {
                    self.emit("{");
                    let mut on_match = |gen: &mut Self| {
                        let guard = arm.guard.as_ref().map(|guard| gen.cond(guard));
                        if let Some(guard) = &guard {
                            gen.emit(format!("if ({guard}) {{"));
                        }
                        gen.branch(&arm.body, result.as_deref(), &ty);
                        gen.emit(format!("goto {end};"));
                        if guard.is_some() {
                            gen.emit("}");
                        }
                    };
                    self.pattern(&arm.pattern, &value, &mut on_match);
                    self.emit("}");
                }
                let fail = self.fail();
                self.emit(format!("nt_panic_text(\"no match arm matched\"); {fail}"));
                self.emit(format!("{end}:;"));
                result.unwrap_or_else(|| zero(repr).into())
            }
            ExprKind::While(cond, body) => {
                let (brk, cont) = (self.label("break"), self.label("continue"));
                self.emit("for (;;) {");
                let mark = self.mark();
                let flag = self.cond(cond);
                self.release(mark);
                self.emit(format!("if (!{flag}) goto {brk};"));
                self.enter_loop(&brk, &cont, None);
                self.loop_body(body);
                self.f.loops.pop();
                self.emit("}");
                self.emit(format!("{brk}:;"));
                "0".into()
            }
            ExprKind::Loop(body) => {
                let (brk, cont) = (self.label("break"), self.label("continue"));
                let result = (repr != Repr::Unit).then(|| self.temp(repr));
                self.emit("for (;;) {");
                self.enter_loop(&brk, &cont, result.clone().map(|r| (r, ty.clone())));
                self.loop_body(body);
                self.f.loops.pop();
                self.emit("}");
                self.emit(format!("{brk}:;"));
                result.unwrap_or_else(|| "0".into())
            }
            ExprKind::For(pattern, iterable, body) => {
                self.for_loop(pattern, iterable, body);
                "0".into()
            }
            ExprKind::Break(value) => {
                let target = self.f.loops.last().map(|l| (l.brk.clone(), l.result.clone(), l.mark, l.cleanups)).unwrap();
                if let (Some(value), Some((result, result_ty))) = (value, &target.1) {
                    let computed = self.expr(value);
                    let computed = self.coerce(&computed, &value.ty, result_ty);
                    let result_repr = self.repr(result_ty);
                    self.assign(result, &computed, result_repr);
                }
                self.release_copy(target.2);
                self.run_cleanups(target.3);
                self.emit(format!("goto {};", target.0));
                "0".into()
            }
            ExprKind::Continue => {
                let target = self.f.loops.last().map(|l| (l.cont.clone(), l.mark, l.cleanups)).unwrap();
                self.release_copy(target.1);
                self.run_cleanups(target.2);
                self.emit(format!("goto {};", target.0));
                "0".into()
            }
            ExprKind::Return(value) => {
                let value = value.as_ref().map(|value| (self.expr(value), value.ty.clone()));
                self.emit_return(value);
                zero(repr).into()
            }
            ExprKind::Throw(value) => {
                let value = self.expr(value);
                let fail = self.fail();
                self.emit(format!("nt_throw({value}); {fail}"));
                zero(repr).into()
            }
            ExprKind::Try(body, catches, finally) => {
                self.try_stmt(body, catches, finally.as_deref());
                "0".into()
            }
            ExprKind::TryExpr(inner) => self.try_expr(inner, &ty),
            ExprKind::Question(inner) => {
                let inner_ty = self.resolve(&inner.ty);
                let value = self.expr(inner);
                let args = self.owner_args(&inner_ty);
                let (good, bad) = if matches!(inner_ty, Ty::Named(def, _) if def == self.result_type) { (self.ok, self.err) } else { (self.some, self.none) };
                let inner_repr = self.repr(&inner_ty);
                let failed = match inner_repr {
                    Repr::OptObj => format!("!{value}"),
                    repr if repr.is_option() => format!("!{value}.tag"),
                    _ => format!("Py_IS_TYPE({value}, (PyTypeObject *)&T{})", self.record(bad, &args)),
                };
                self.emit(format!("if ({failed}) {{"));
                let ret_ty = match &self.f.ret {
                    Ret::Native(ty) | Ret::Boxed(ty) => self.resolve(ty),
                    Ret::Status => Ty::Dyn,
                };
                let replacement = if bad == self.err {
                    let payload = self.field(&value, bad, 0, &inner_ty, &args[1]);
                    self.make_record(bad, &self.owner_args(&ret_ty), &[payload])
                } else {
                    self.option_value(&ret_ty, None)
                };
                self.emit_return(Some((replacement, ret_ty)));
                self.emit("}");
                match inner_repr {
                    Repr::OptObj => value,
                    repr if repr.is_option() => format!("{value}.value"),
                    _ => self.field(&value, good, 0, &inner_ty, &ty),
                }
            }
            ExprKind::With(manager, local, body) => {
                self.with_stmt(manager, *local, body);
                "0".into()
            }
            ExprKind::Await(inner) => {
                let value = self.expr(inner);
                let result = self.await_value(&value);
                self.checked_value(&result, &ty, "")
            }
            ExprKind::SetLocal(local, value) => {
                let result = self.expr(value);
                self.write_local(*local, &result, &value.ty, false);
                "0".into()
            }
            ExprKind::SetIndex(object, index, value) => {
                let object_ty = self.resolve(&object.ty);
                let object = self.expr(object);
                let index_value = self.expr(index);
                let result = self.expr(value);
                if let Ty::Map(..) = object_ty {
                    let key = self.box_value(&index_value, &index.ty);
                    let boxed = self.box_value(&result, &value.ty);
                    self.check(&format!("PyDict_SetItem({object}, {key}, {boxed}) < 0"));
                } else {
                    let setter = match self.storage(&value.ty) {
                        Repr::I64 => "nt_list_set_i64",
                        Repr::F64 => "nt_list_set_f64",
                        Repr::Bool => "nt_list_set_bool",
                        _ => "nt_list_set",
                    };
                    let result = self.stored(&result, &value.ty);
                    self.check(&format!("{setter}({object}, {index_value}, {result}) < 0"));
                }
                "0".into()
            }
            ExprKind::SetPyIndex(object, parts, value) => {
                let object = self.expr(object);
                let key = self.index_key(parts);
                let result = self.expr(value);
                self.check(&format!("PyObject_SetItem({object}, {key}, {result}) < 0"));
                "0".into()
            }
            ExprKind::SetAttr(object, name, value) => {
                let object = self.expr(object);
                let result = self.expr(value);
                let name = self.string(name);
                self.check(&format!("PyObject_SetAttr({object}, {name}, {result}) < 0"));
                "0".into()
            }
        }
    }

    fn branch(&mut self, value: &Expr, result: Option<&str>, ty: &Ty) {
        let computed = self.expr(value);
        if let Some(result) = result {
            if !matches!(self.resolve(&value.ty), Ty::Never) {
                let converted = self.coerce(&computed, &value.ty, ty);
                let repr = self.repr(ty);
                self.assign(result, &converted, repr);
            }
        }
    }

    fn list_get(&mut self, list: &str, index: &str, item_ty: &Ty) -> String {
        let (repr, storage) = (self.repr(item_ty), self.storage(item_ty));
        let getter = match storage {
            Repr::I64 => "nt_list_get_i64",
            Repr::F64 => "nt_list_get_f64",
            Repr::Bool => "nt_list_get_bool",
            _ => "nt_list_get_obj",
        };
        let result = self.call(if storage == Repr::Unit { Repr::Obj } else { storage }, format!("{getter}({list}, {index})"));
        match repr {
            Repr::Unit => "0".into(),
            repr if repr.is_option() => self.unbox(&result, repr),
            _ => result,
        }
    }

    fn field(&mut self, object: &str, def: DefId, index: usize, owner: &Ty, ty: &Ty) -> String {
        let args = self.owner_args(owner);
        let declared = self.p.fields(def)[index].ty.subst(&args);
        let repr = self.repr(&declared);
        if repr == Repr::Unit {
            return "0".into();
        }
        let record = self.record(def, &args);
        let stored = format!("((S{record} *){object})->f{index}");
        let value = if repr.is_option() { self.unbox(&stored, repr) } else { stored };
        self.coerce(&value, &declared, ty)
    }

    fn argv(&mut self, values: &[String]) -> String {
        if values.is_empty() {
            return "NULL".into();
        }
        self.f.temps += 1;
        let name = format!("argv{}", self.f.temps);
        self.emit(format!("PyObject *{name}[] = {{{}}};", values.join(", ")));
        name
    }

    fn vectorcall(&mut self, function: &str, values: &[String], kwnames: Option<String>) -> String {
        let argv = self.argv(values);
        let positional = values.len() - kwnames.as_ref().map_or(0, |k| self.kwnames[k[2..].parse::<usize>().unwrap()].len());
        let kwnames = kwnames.unwrap_or_else(|| "NULL".into());
        self.call(Repr::Obj, format!("PyObject_Vectorcall({function}, {argv}, {positional}, {kwnames})"))
    }

    fn py_call(&mut self, function: &str, args: &[PyArg]) -> String {
        let spread = args.iter().any(|a| matches!(a, PyArg::Star(_) | PyArg::StarStar(_)));
        if spread {
            let list = self.call(Repr::Obj, "PyList_New(0)".into());
            let dict = self.call(Repr::Obj, "PyDict_New()".into());
            for arg in args {
                match arg {
                    PyArg::Pos(value) => {
                        let value = self.expr(value);
                        self.check(&format!("PyList_Append({list}, {value}) < 0"));
                    }
                    PyArg::Star(value) => {
                        let value = self.expr(value);
                        self.call(Repr::Obj, format!("PySequence_InPlaceConcat({list}, {value})"));
                    }
                    PyArg::Named(name, value) => {
                        let value = self.expr(value);
                        let name = self.string(name);
                        self.check(&format!("PyDict_SetItem({dict}, {name}, {value}) < 0"));
                    }
                    PyArg::StarStar(value) => {
                        let value = self.expr(value);
                        self.check(&format!("PyDict_Update({dict}, {value}) < 0"));
                    }
                }
            }
            return self.call(Repr::Obj, format!("nt_call_spread({function}, {list}, {dict})"));
        }
        let mut positional = vec![];
        let mut named = vec![];
        let mut names = vec![];
        for arg in args {
            match arg {
                PyArg::Pos(value) => positional.push(self.expr(value)),
                PyArg::Named(name, value) => {
                    named.push(self.expr(value));
                    names.push(name.clone());
                }
                _ => unreachable!(),
            }
        }
        let kwnames = (!names.is_empty()).then(|| {
            self.kwnames.push(names);
            format!("KW{}", self.kwnames.len() - 1)
        });
        positional.extend(named);
        self.vectorcall(function, &positional, kwnames)
    }

    fn index_key(&mut self, parts: &[PyIndexPart]) -> String {
        let values: Vec<String> = parts
            .iter()
            .map(|part| match part {
                PyIndexPart::Expr(value) => self.expr(value),
                PyIndexPart::Ellipsis => "Py_Ellipsis".into(),
                PyIndexPart::Slice(start, stop, step) => {
                    let bound = |gen: &mut Self, value: &Option<Box<Expr>>| value.as_ref().map_or("Py_None".to_string(), |v| gen.expr(v));
                    let (start, stop, step) = (bound(self, start), bound(self, stop), bound(self, step));
                    self.call(Repr::Obj, format!("PySlice_New({start}, {stop}, {step})"))
                }
            })
            .collect();
        if values.len() == 1 {
            return values[0].clone();
        }
        let tuple = self.call(Repr::Obj, format!("PyTuple_New({})", values.len()));
        for (index, value) in values.iter().enumerate() {
            self.emit(format!("PyTuple_SET_ITEM({tuple}, {index}, Py_NewRef({value}));"));
        }
        tuple
    }

    fn binary(&mut self, op: BinOp, left: &Expr, right: &Expr, ty: &Ty) -> String {
        if matches!(op, BinOp::And | BinOp::Or) {
            let result = self.temp(Repr::Bool);
            let first = self.expr(left);
            self.emit(format!("{result} = {first};"));
            self.emit(format!("if ({}{result}) {{", if op == BinOp::And { "" } else { "!" }));
            let second = self.expr(right);
            self.emit(format!("{result} = {second};"));
            self.emit("}");
            return result;
        }
        let operand = self.resolve(&left.ty);
        let a = self.expr(left);
        let b = self.expr(right);
        if op == BinOp::In {
            let item = self.box_value(&a, &left.ty);
            return self.call(Repr::Bool, format!("PySequence_Contains({b}, {item})"));
        }
        if operand == Ty::Dyn || self.resolve(&right.ty) == Ty::Dyn {
            return self.object_binary(op, &a, &b, ty);
        }
        // Unboxed options are compared as records.
        let (a, b) = (self.stored(&a, &left.ty), self.stored(&b, &right.ty));
        match self.storage(&left.ty) {
            Repr::I64 => self.int_binary(op, &a, &b),
            Repr::F64 => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => format!("({a} {} {b})", op.text()),
                BinOp::FloorDiv => format!("nt_floordiv_f64({a}, {b})"),
                BinOp::Mod => format!("nt_mod_f64({a}, {b})"),
                BinOp::Pow => format!("pow({a}, {b})"),
                _ => format!("({a} {} {b})", op.text()),
            },
            Repr::Bool | Repr::Unit => format!("({a} {} {b})", op.text()),
            _ => {
                if op == BinOp::Add && matches!(operand, Ty::List(_)) {
                    return self.call(Repr::Obj, format!("nt_list_concat({a}, {b})"));
                }
                let result = self.object_binary(op, &a, &b, &Ty::Dyn);
                if op.is_comparison() {
                    let flag = self.call(Repr::Bool, format!("PyObject_IsTrue({result})"));
                    return flag;
                }
                self.unbox(&result, self.repr(ty))
            }
        }
    }

    fn int_binary(&mut self, op: BinOp, a: &str, b: &str) -> String {
        let checked = |gen: &mut Self, call: String| {
            let result = gen.temp(Repr::I64);
            gen.check(&format!("{} < 0", call.replace("OUT", &format!("&{result}"))));
            result
        };
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let builtin = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    _ => "mul",
                };
                let result = self.temp(Repr::I64);
                let fail = self.fail();
                self.emit(format!("if (__builtin_{builtin}_overflow({a}, {b}, &{result})) {{ nt_overflow(); {fail} }}"));
                result
            }
            BinOp::Div => format!("((double){a} / (double){b})"),
            BinOp::FloorDiv => checked(self, format!("nt_floordiv_i64({a}, {b}, OUT)")),
            BinOp::Mod => checked(self, format!("nt_mod_i64({a}, {b}, OUT)")),
            BinOp::Pow => checked(self, format!("nt_pow_i64({a}, {b}, OUT)")),
            _ => format!("({a} {} {b})", op.text()),
        }
    }

    fn object_binary(&mut self, op: BinOp, a: &str, b: &str, ty: &Ty) -> String {
        let compare = match op {
            BinOp::Eq => Some("Py_EQ"),
            BinOp::Ne => Some("Py_NE"),
            BinOp::Lt => Some("Py_LT"),
            BinOp::Le => Some("Py_LE"),
            BinOp::Gt => Some("Py_GT"),
            BinOp::Ge => Some("Py_GE"),
            _ => None,
        };
        if let Some(compare) = compare {
            return self.call(Repr::Obj, format!("PyObject_RichCompare({a}, {b}, {compare})"));
        }
        let extra = if op == BinOp::Pow { ", Py_None" } else { "" };
        let result = self.call(Repr::Obj, format!("{}({a}, {b}{extra})", py_number(op)));
        if self.repr(ty) == Repr::Obj { result } else { self.unbox(&result, self.repr(ty)) }
    }

    fn enter_loop(&mut self, brk: &str, cont: &str, result: Option<(String, Ty)>) {
        let (mark, cleanups) = (self.mark(), self.f.cleanups.len());
        self.f.loops.push(LoopCtx { brk: brk.into(), cont: cont.into(), result, mark, cleanups });
    }

    fn loop_body(&mut self, body: &Expr) {
        let mark = self.mark();
        self.expr(body);
        self.release(mark);
        let cont = self.f.loops.last().unwrap().cont.clone();
        self.emit(format!("{cont}:;"));
    }

    fn for_loop(&mut self, pattern: &Pattern, iterable: &Expr, body: &Expr) {
        let (brk, cont) = (self.label("break"), self.label("continue"));
        let item_ty = pattern.ty.clone();
        if let ExprKind::Range(start, end, inclusive) = &iterable.kind {
            let start = self.expr(start);
            let end = self.expr(end);
            let (counter, limit) = (self.temp(Repr::I64), self.temp(Repr::I64));
            self.emit(format!("{limit} = {end}{};", if *inclusive { " + 1" } else { "" }));
            self.emit(format!("for ({counter} = {start}; {counter} < {limit}; {counter}++) {{"));
            self.enter_loop(&brk, &cont, None);
            self.loop_item(pattern, &counter, &Ty::Int, body);
            self.f.loops.pop();
            self.emit("}");
        } else if let Ty::List(_) = self.resolve(&iterable.ty) {
            let list = self.expr(iterable);
            let index = self.temp(Repr::I64);
            self.emit(format!("for ({index} = 0; {index} < nt_list_len({list}); {index}++) {{"));
            self.enter_loop(&brk, &cont, None);
            let mark = self.mark();
            let item = self.list_get(&list, &index, &item_ty);
            self.loop_item(pattern, &item, &item_ty, body);
            self.release(mark);
            self.f.loops.pop();
            self.emit("}");
        } else {
            let iterable_value = self.expr(iterable);
            let iterator = self.call(Repr::Obj, format!("PyObject_GetIter({iterable_value})"));
            self.emit("for (;;) {");
            self.enter_loop(&brk, &cont, None);
            let mark = self.mark();
            let item = self.temp(Repr::Obj);
            let fail = self.fail();
            self.emit(format!("{item} = PyIter_Next({iterator});"));
            self.emit(format!("if (!{item}) {{ if (PyErr_Occurred()) {fail} goto {brk}; }}"));
            let repr = self.repr(&item_ty);
            let value = self.unbox(&item, repr);
            self.loop_item(pattern, &value, &item_ty, body);
            self.release(mark);
            self.f.loops.pop();
            self.emit("}");
        }
        self.emit(format!("{brk}:;"));
    }

    fn loop_item(&mut self, pattern: &Pattern, item: &str, item_ty: &Ty, body: &Expr) {
        let item = self.coerce(item, item_ty, &pattern.ty);
        self.pattern(pattern, &item, &mut |gen| gen.loop_body(body));
    }

    fn region_temps(&mut self, from: usize) {
        let clears: Vec<String> = self.f.created[from..].iter().map(|name| format!("Py_CLEAR({name});")).collect();
        if !clears.is_empty() {
            self.emit(clears.join(" "));
        }
    }

    fn try_stmt(&mut self, body: &Expr, catches: &[Catch], finally: Option<&Expr>) {
        let (catch_label, end) = (self.label("catch"), self.label("try_end"));
        let exception = self.temp(Repr::Obj);
        let region = self.f.created.len();
        let outer_error = std::mem::replace(&mut self.f.error, catch_label.clone());
        if let Some(finally) = finally {
            self.f.cleanups.push(Cleanup::Finally(finally.clone()));
        }
        self.stmt(body);
        if finally.is_some() {
            self.f.cleanups.pop();
        }
        self.f.error = outer_error;
        if let Some(finally) = finally {
            self.stmt(finally);
        }
        self.emit(format!("goto {end};"));
        self.emit(format!("{catch_label}:"));
        self.region_temps(region);
        self.emit(format!("{exception} = PyErr_GetRaisedException();"));
        self.emit("if (0) {");
        for catch in catches {
            let filters: Vec<String> = catch.filters.iter().map(|f| self.expr(f)).collect();
            let condition = if filters.is_empty() {
                format!("nt_matches({exception}, NULL)")
            } else {
                filters.iter().map(|f| format!("nt_matches({exception}, {f})")).collect::<Vec<_>>().join(" || ")
            };
            self.emit(format!("}} else if ({condition}) {{"));
            let error = self.call(Repr::Obj, format!("nt_make_pyerr({exception})"));
            self.emit(format!("Py_CLEAR({exception});"));
            self.write_local(catch.local, &error, &Ty::Dyn, true);
            if let Some(finally) = finally {
                self.f.cleanups.push(Cleanup::Finally(finally.clone()));
            }
            self.stmt(&catch.body);
            if finally.is_some() {
                self.f.cleanups.pop();
            }
            if let Some(finally) = finally {
                self.stmt(finally);
            }
            self.emit(format!("goto {end};"));
        }
        self.emit("}");
        if let Some(finally) = finally {
            self.stmt(finally);
        }
        let fail = self.fail();
        self.emit(format!("PyErr_SetRaisedException({exception}); {exception} = NULL; {fail}"));
        self.emit(format!("{end}:;"));
    }

    fn try_expr(&mut self, inner: &Expr, ty: &Ty) -> String {
        let args = self.owner_args(ty);
        let (catch_label, end) = (self.label("catch"), self.label("try_end"));
        let result = self.temp(Repr::Obj);
        let exception = self.temp(Repr::Obj);
        let region = self.f.created.len();
        let outer_error = std::mem::replace(&mut self.f.error, catch_label.clone());
        let value = self.expr(inner);
        let value = self.coerce(&value, &inner.ty, &args[0]);
        self.f.error = outer_error;
        let ok = self.make_record(self.ok, &args, &[value]);
        self.emit(format!("{result} = Py_NewRef({ok});"));
        self.emit(format!("goto {end};"));
        self.emit(format!("{catch_label}:"));
        self.region_temps(region);
        self.emit(format!("{exception} = PyErr_GetRaisedException();"));
        let fail = self.fail();
        self.emit(format!("if (!nt_matches({exception}, NULL)) {{ PyErr_SetRaisedException({exception}); {exception} = NULL; {fail} }}"));
        let error = self.call(Repr::Obj, format!("nt_make_pyerr({exception})"));
        let err = self.make_record(self.err, &args, &[error]);
        self.emit(format!("{result} = Py_NewRef({err});"));
        self.emit(format!("{end}:;"));
        result
    }

    fn with_stmt(&mut self, manager: &Expr, local: Option<usize>, body: &Expr) {
        let manager_value = self.expr(manager);
        let exit_name = self.string("__exit__");
        let enter_name = self.string("__enter__");
        let exit = self.call(Repr::Obj, format!("PyObject_GetAttr({manager_value}, {exit_name})"));
        let entered = self.call(Repr::Obj, format!("PyObject_CallMethodNoArgs({manager_value}, {enter_name})"));
        if let Some(local) = local {
            let ty = self.body().locals[local].ty.clone();
            let value = self.checked_value(&entered, &ty, "");
            self.write_local(local, &value, &ty, true);
        }
        let (catch_label, end) = (self.label("with_error"), self.label("with_end"));
        let exception = self.temp(Repr::Obj);
        let region = self.f.created.len();
        let outer_error = std::mem::replace(&mut self.f.error, catch_label.clone());
        self.f.cleanups.push(Cleanup::WithExit(exit.clone()));
        self.stmt(body);
        self.f.cleanups.pop();
        self.f.error = outer_error;
        self.call(Repr::Obj, format!("nt_with_exit({exit}, NULL)"));
        self.emit(format!("goto {end};"));
        self.emit(format!("{catch_label}:"));
        self.region_temps(region);
        self.emit(format!("{exception} = PyErr_GetRaisedException();"));
        let outcome = self.temp(Repr::Obj);
        let fail = self.fail();
        self.emit(format!("{outcome} = nt_with_exit({exit}, {exception});"));
        self.emit(format!("if (!{outcome}) {fail}"));
        self.emit(format!("if (!PyObject_IsTrue({outcome})) {{ PyErr_SetRaisedException({exception}); {exception} = NULL; {fail} }}"));
        self.emit(format!("{end}:;"));
    }

    // Patterns: tests become nested ifs, on_match runs inside the innermost one.

    fn pattern(&mut self, pattern: &Pattern, value: &str, on_match: &mut dyn FnMut(&mut Self)) {
        match &pattern.kind {
            PatKind::Wild => on_match(self),
            PatKind::Bind(local) => {
                self.write_local(*local, value, &pattern.ty, true);
                on_match(self);
            }
            PatKind::Lit(literal) => {
                let expected = self.expr(literal);
                let condition = match self.repr(&pattern.ty) {
                    Repr::Obj => format!("PyObject_RichCompareBool({value}, {expected}, Py_EQ) == 1"),
                    _ => format!("{value} == {expected}"),
                };
                self.emit(format!("if ({condition}) {{"));
                on_match(self);
                self.emit("}");
            }
            PatKind::Variant(def, fields) if self.repr(&pattern.ty).is_option() => {
                let repr = self.repr(&pattern.ty);
                let (present, payload) = if repr == Repr::OptObj { (value.to_string(), value.to_string()) } else { (format!("{value}.tag"), format!("{value}.value")) };
                let negate = if *def == self.some { "" } else { "!" };
                self.emit(format!("if ({negate}{present}) {{"));
                let items: Vec<(&Pattern, String)> = fields.iter().map(|(_, sub)| (sub, payload.clone())).collect();
                self.pattern_list(&items, on_match);
                self.emit("}");
            }
            PatKind::Variant(def, fields) => {
                let record = self.record(*def, &self.owner_args(&pattern.ty));
                let test = matches!(self.p.defs[*def].kind, DefKind::Variant { .. });
                if test {
                    self.emit(format!("if (Py_IS_TYPE({value}, (PyTypeObject *)&T{record})) {{"));
                }
                let items: Vec<(&Pattern, String)> = fields.iter().map(|(index, sub)| (sub, self.field(value, *def, *index, &pattern.ty, &sub.ty))).collect();
                self.pattern_list(&items, on_match);
                if test {
                    self.emit("}");
                }
            }
            PatKind::Tuple(items) => {
                let values: Vec<(&Pattern, String)> = items.iter().enumerate().map(|(index, sub)| {
                    let repr = self.repr(&sub.ty);
                    (sub, self.unbox(&format!("PyTuple_GET_ITEM({value}, {index})"), repr))
                }).collect();
                self.pattern_list(&values, on_match);
            }
            PatKind::List(items, rest) => {
                let comparison = if *rest { ">=" } else { "==" };
                self.emit(format!("if (nt_list_len({value}) {comparison} {}) {{", items.len()));
                let values: Vec<(&Pattern, String)> = items.iter().enumerate().map(|(index, sub)| {
                    let item = self.call(Repr::Obj, format!("nt_list_get_obj({value}, {index})"));
                    let repr = self.repr(&sub.ty);
                    (sub, self.unbox(&item, repr))
                }).collect();
                self.pattern_list(&values, on_match);
                self.emit("}");
            }
            PatKind::Or(alternatives) => {
                let matched = self.temp(Repr::Bool);
                for alternative in alternatives {
                    self.emit(format!("if (!{matched}) {{"));
                    self.pattern(alternative, value, &mut |gen| gen.emit(format!("{matched} = 1;")));
                    self.emit("}");
                }
                self.emit(format!("if ({matched}) {{"));
                on_match(self);
                self.emit("}");
            }
            PatKind::TypeTest(local) => {
                let ty = self.body().locals[*local].ty.clone();
                let spec = self.spec(&ty);
                let converted = self.temp(Repr::Obj);
                self.emit(format!("{converted} = nt_try_convert({value}, {});", c_string(&spec)));
                self.emit(format!("if ({converted}) {{"));
                let repr = self.repr(&ty);
                let unboxed = self.unbox(&converted, repr);
                self.write_local(*local, &unboxed, &ty, true);
                on_match(self);
                self.emit("}");
            }
            PatKind::PyNone => {
                self.emit(format!("if ({value} == Py_None) {{"));
                on_match(self);
                self.emit("}");
            }
        }
    }

    fn pattern_list(&mut self, items: &[(&Pattern, String)], on_match: &mut dyn FnMut(&mut Self)) {
        match items.split_first() {
            None => on_match(self),
            Some(((pattern, value), rest)) => self.pattern(pattern, value, &mut |gen| gen.pattern_list(rest, on_match)),
        }
    }

    // Module:

    fn run(&mut self) -> String {
        let p = self.p;
        self.record(self.pyerr, &[]);
        for body in &p.fns {
            if p.sig(body.def).generics.is_empty() {
                self.instance(body.def, &[]);
            }
        }
        self.drain();
        self.setup();
        self.drain();
        let types = self.types();
        let mut head = String::from("#include \"nicethon.h\"\n\nstatic int nt_ready(void);\n");
        for index in 0..self.strings.len() {
            writeln!(head, "static PyObject *K{index};").unwrap();
        }
        for index in 0..self.kwnames.len() {
            writeln!(head, "static PyObject *KW{index};").unwrap();
        }
        for (index, global) in p.globals.iter().enumerate() {
            let repr = global.ty.repr();
            let repr = if repr == Repr::Unit { Repr::Obj } else { repr };
            writeln!(head, "static {} G{index};", c_type(repr)).unwrap();
        }
        let mut decls = String::new();
        for (instance, (def, _)) in self.instances.iter().enumerate() {
            writeln!(decls, "{};\n{};\nstatic PyObject *W{instance}(PyObject *, PyObject *const *, Py_ssize_t, PyObject *);\nstatic PyObject *V{instance};", self.native_header(instance), self.adapter_header(instance)).unwrap();
            for lambda in 0..p.fns[self.bodies[def]].lambdas.len() {
                let (params, ret) = self.lambda_types(instance, lambda);
                writeln!(decls, "{};\nstatic PyObject *L{instance}_{lambda}(PyObject *, PyObject *const *, Py_ssize_t, PyObject *);", self.native_signature(&format!("N{instance}_{lambda}"), &params, &ret)).unwrap();
            }
        }
        let ready = self.ready();
        format!("{head}\n{types}\n{decls}\n{}{ready}", self.code)
    }

    fn drain(&mut self) {
        while let Some(instance) = self.pending.pop() {
            self.native(instance);
        }
    }

    // Writing a descriptor can create more record instances, so loop until done.
    fn types(&mut self) -> String {
        let mut text = String::new();
        let mut index = 0;
        while index < self.records.len() {
            let (def, args) = self.records[index].clone();
            let p = self.p;
            let info = &p.defs[def];
            let fields = p.fields(def);
            let mut members = String::new();
            let mut descriptors = String::new();
            for (position, field) in fields.iter().enumerate() {
                let ty = field.ty.subst(&args);
                let repr = match self.storage(&ty) {
                    Repr::Unit => Repr::Obj,
                    repr => repr,
                };
                write!(members, " {} f{position};", c_type(repr)).unwrap();
                let name = field.name.clone().unwrap_or_default();
                let spec = self.spec(&ty);
                write!(descriptors, "{{{}, {}, offsetof(S{index}, f{position}), {}, {}}}, ", c_string(&name), kind(repr), c_string(&spec), c_string(&self.show(&ty))).unwrap();
            }
            let (qualname, enum_id) = match &info.kind {
                DefKind::Variant { parent, .. } => (format!("{}.{}.{}", self.module, p.defs[*parent].name, info.name), self.enum_id(*parent, &args) as i64),
                _ => (format!("{}.{}", self.module, info.name), -1),
            };
            writeln!(text, "typedef struct {{ PyObject_HEAD{members} }} S{index};").unwrap();
            let descriptor = if fields.is_empty() {
                writeln!(text, "static PyObject *U{index};").unwrap();
                "NULL".to_string()
            } else {
                writeln!(text, "static const nt_Field D{index}[] = {{{descriptors}}};").unwrap();
                format!("D{index}")
            };
            writeln!(text, "static nt_RecordType T{index} = NT_RECORD_TYPE({}, {}, S{index}, {descriptor}, {}, {enum_id});", c_string(&qualname), c_string(&info.name), fields.len()).unwrap();
            index += 1;
        }
        text
    }

    fn ready(&self) -> String {
        let mut text = format!("static PyTypeObject *nt_type_table[{}];\n\nstatic int nt_ready(void) {{\n", self.records.len() + 1);
        for (index, (def, _)) in self.records.iter().enumerate() {
            writeln!(text, "    if (PyType_Ready((PyTypeObject *)&T{index}) < 0) return -1;\n    nt_type_table[{index}] = (PyTypeObject *)&T{index};").unwrap();
            if self.p.fields(*def).is_empty() {
                writeln!(text, "    if (!(U{index} = nt_record_alloc((PyTypeObject *)&T{index}))) return -1;").unwrap();
            }
        }
        writeln!(text, "    nt_types = nt_type_table;\n    nt_pyerr_type = (PyTypeObject *)&T{};", self.record_index[&(self.pyerr, vec![])]).unwrap();
        for (instance, (def, _)) in self.instances.iter().enumerate() {
            let name = c_string(&self.p.defs[*def].name);
            writeln!(text, "    if (!(V{instance} = nt_closure_new(W{instance}, (void *)NF{instance}, NULL, {name}))) return -1;").unwrap();
        }
        text.push_str("    return 0;\n}\n");
        text
    }

    fn setup(&mut self) {
        let entry = self.p.entry_module;
        let module_name = self.module.to_string();
        let setup = |gen: &mut Self| {
            gen.emit("if (nt_init_runtime() < 0 || nt_ready() < 0) goto error;");
            let module_object = gen.call(Repr::Obj, format!("PyUnicode_FromString({})", c_string(&module_name)));
            let mut order: Vec<usize> = (0..gen.p.globals.len()).collect();
            order.sort_by_key(|&index| match gen.p.globals[index].init {
                GlobalInit::Import(_) => 0,
                GlobalInit::Attr(..) => 1,
                GlobalInit::Value(_) => 2,
                GlobalInit::Class(_) => 3,
                GlobalInit::Decorated(..) => 4,
            });
            for index in order {
                let global = &gen.p.globals[index];
                gen.set_line(global.pos);
                gen.f.module = global.module;
                let value = match &global.init {
                    GlobalInit::Import(name) => gen.call(Repr::Obj, format!("PyImport_ImportModule({})", c_string(name))),
                    GlobalInit::Attr(module, name) => {
                        let attr = gen.string(name);
                        let raw = gen.call(Repr::Obj, format!("PyObject_GetAttr(G{module}, {attr})"));
                        let value = gen.checked_value(&raw, &global.ty, name);
                        gen.stored(&value, &global.ty)
                    }
                    GlobalInit::Value(value) => {
                        let result = gen.expr(value);
                        let result = gen.coerce(&result, &value.ty, &global.ty);
                        gen.stored(&result, &global.ty)
                    }
                    GlobalInit::Class(def) => gen.py_class(*def, &module_object),
                    GlobalInit::Decorated(def, decorators) => {
                        let mut current = format!("V{}", gen.instance(*def, &[]));
                        for decorator in decorators {
                            let function = gen.expr(decorator);
                            current = gen.vectorcall(&function, &[current], None);
                        }
                        gen.checked_value(&current, &global.ty, "")
                    }
                };
                let repr = global.ty.repr();
                match repr {
                    Repr::Obj | Repr::Unit => gen.emit(format!("G{index} = Py_NewRef({value});")),
                    _ => gen.emit(format!("G{index} = {value};")),
                }
            }
            gen.f.module = entry;
            gen.exports();
        };
        let header = "static int nt_setup(PyObject *module)".to_string();
        let mut kwnames_setup = String::new();
        let mut strings_setup = String::new();
        self.function(header, "<module>", Func { module: entry, ..Default::default() }, setup, None);
        for (index, text) in self.strings.iter().enumerate() {
            writeln!(strings_setup, "    K{index} = PyUnicode_InternFromString({});\n    if (!K{index}) return -1;", c_string(text)).unwrap();
        }
        for (index, names) in self.kwnames.iter().enumerate() {
            let items: Vec<String> = names.iter().map(|n| format!("PyUnicode_InternFromString({})", c_string(n))).collect();
            writeln!(kwnames_setup, "    KW{index} = PyTuple_Pack({}, {});\n    if (!KW{index}) return -1;", items.len(), items.join(", ")).unwrap();
        }
        let module = self.module;
        write!(
            self.code,
            "static int nt_constants(void) {{\n{strings_setup}{kwnames_setup}    return 0;\n}}\n\n\
             static struct PyModuleDef nt_module = {{PyModuleDef_HEAD_INIT, {}, NULL, -1, NULL}};\n\n\
             PyMODINIT_FUNC PyInit_{module}(void) {{\n    PyObject *module = PyModule_Create(&nt_module);\n    if (!module) return NULL;\n    if (nt_constants() < 0 || nt_setup(module) < 0) {{\n        Py_DECREF(module);\n        return NULL;\n    }}\n    return module;\n}}\n",
            c_string(module)
        )
        .unwrap();
    }

    fn py_class(&mut self, def: DefId, module_object: &str) -> String {
        let DefKind::PyClass { methods, bases, .. } = &self.p.defs[def].kind else { unreachable!() };
        let dict = self.call(Repr::Obj, "PyDict_New()".into());
        let module_key = self.string("__module__");
        self.check(&format!("PyDict_SetItem({dict}, {module_key}, {module_object}) < 0"));
        for (name, method) in methods {
            let key = self.string(name);
            let function = format!("V{}", self.instance(*method, &[]));
            let function = self.call(Repr::Obj, format!("PyInstanceMethod_New({function})"));
            self.check(&format!("PyDict_SetItem({dict}, {key}, {function}) < 0"));
        }
        let values: Vec<String> = bases.iter().map(|b| self.expr(b)).collect();
        let tuple = self.call(Repr::Obj, format!("PyTuple_New({})", values.len()));
        for (index, value) in values.iter().enumerate() {
            self.emit(format!("PyTuple_SET_ITEM({tuple}, {index}, Py_NewRef({value}));"));
        }
        let meta = values.first().map_or("(PyObject *)&PyType_Type".to_string(), |b| format!("(PyObject *)Py_TYPE({b})"));
        let name = c_string(&self.p.defs[def].name);
        self.call(Repr::Obj, format!("PyObject_CallFunction({meta}, \"sOO\", {name}, {tuple}, {dict})"))
    }

    fn exports(&mut self) {
        let entry = self.p.entry_module;
        for (def, info) in self.p.defs.iter().enumerate() {
            if info.module != entry || !info.export {
                continue;
            }
            let generic = |owner: DefId| matches!(&self.p.defs[owner].kind, DefKind::Struct { generics, .. } | DefKind::Enum { generics, .. } if !generics.is_empty());
            let value = match &info.kind {
                DefKind::Fn(sig) if sig.kind == FnKind::Native && sig.owner.is_none() => format!("V{}", self.instance(def, &[])),
                DefKind::Struct { .. } if !generic(def) => format!("(PyObject *)&T{}", self.record(def, &[])),
                DefKind::Variant { parent, .. } if !generic(*parent) => format!("(PyObject *)&T{}", self.record(def, &[])),
                DefKind::PyClass { global, .. } => format!("G{global}"),
                _ => continue,
            };
            self.check(&format!("PyModule_AddObjectRef(module, {}, {value}) < 0", c_string(&info.name)));
        }
    }
}

fn py_number(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "PyNumber_Add",
        BinOp::Sub => "PyNumber_Subtract",
        BinOp::Mul => "PyNumber_Multiply",
        BinOp::Div => "PyNumber_TrueDivide",
        BinOp::FloorDiv => "PyNumber_FloorDivide",
        BinOp::Mod => "PyNumber_Remainder",
        BinOp::Pow => "PyNumber_Power",
        BinOp::MatMul => "PyNumber_MatrixMultiply",
        BinOp::BitAnd => "PyNumber_And",
        BinOp::BitOr => "PyNumber_Or",
        BinOp::BitXor => "PyNumber_Xor",
        BinOp::Shl => "PyNumber_Lshift",
        BinOp::Shr => "PyNumber_Rshift",
        _ => unreachable!("not a number operator"),
    }
}
