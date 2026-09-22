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
}
