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
}
