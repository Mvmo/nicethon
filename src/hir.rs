use crate::ast::{self, BinOp, Pos, UnOp};

pub type DefId = usize;

// A trait bound with the trait's type arguments, e.g. Iterator<Int>.
pub type Bound = (DefId, Vec<Ty>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Bytes,
    BigInt,
    Unit,
    Never,
    Dyn,
    Error,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Tuple(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Coroutine(Box<Ty>),
    Named(DefId, Vec<Ty>),
    Param(u32),
    Var(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Repr {
    I64,
    F64,
    Bool,
    Unit,
    Obj,
}

impl Repr {
    // Holds a reference that must be released (OptObj may also be NULL).
    pub fn owned(self) -> bool {
        matches!(self, Repr::Obj | Repr::OptObj)
    }
}

impl Ty {
    pub fn repr(&self) -> Repr {
        match self {
            Ty::Int => Repr::I64,
            Ty::Float => Repr::F64,
            Ty::Bool => Repr::Bool,
            Ty::Unit | Ty::Never => Repr::Unit,
            _ => Repr::Obj,
        }
    }

    pub fn map(&self, f: &mut impl FnMut(&Ty) -> Option<Ty>) -> Ty {
        if let Some(ty) = f(self) {
            return ty;
        }
        let mut each = |items: &[Ty]| items.iter().map(|ty| ty.map(f)).collect::<Vec<_>>();
        match self {
            Ty::List(item) => Ty::List(Box::new(item.map(f))),
            Ty::Set(item) => Ty::Set(Box::new(item.map(f))),
            Ty::Coroutine(item) => Ty::Coroutine(Box::new(item.map(f))),
            Ty::Map(key, value) => Ty::Map(Box::new(key.map(f)), Box::new(value.map(f))),
            Ty::Tuple(items) => Ty::Tuple(each(items)),
            Ty::Fn(params, ret) => {
                let params = each(params);
                Ty::Fn(params, Box::new(ret.map(f)))
            }
            Ty::Named(def, args) => Ty::Named(*def, each(args)),
            other => other.clone(),
        }
    }

    pub fn subst(&self, args: &[Ty]) -> Ty {
        self.map(&mut |ty| match ty {
            Ty::Param(index) => args.get(*index as usize).cloned(),
            _ => None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub ty: Ty,
    pub kind: ExprKind,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    Unit,
    Local(usize),
    Global(usize),
    FnRef(DefId, Vec<Ty>),
    Lambda(usize),
    Call(DefId, Vec<Ty>, Vec<Expr>),
    CallValue(Box<Expr>, Vec<Expr>),
    PyCall(Box<Expr>, Vec<PyArg>),
    PyAttr(Box<Expr>, String),
    PyIndex(Box<Expr>, Vec<PyIndexPart>),
    TraitCall(DefId, Vec<Ty>, Vec<Expr>),
    Super(usize, usize),
    ToDyn(Box<Expr>),
    FromDyn(Box<Expr>, bool),
    Construct(DefId, Vec<Expr>),
    Field(Box<Expr>, DefId, usize),
    CopyWith(Box<Expr>, DefId, Vec<(usize, Expr)>),
    TupleGet(Box<Expr>, usize),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Range(Box<Expr>, Box<Expr>, bool),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Inplace(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Cast(Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Template(Vec<Expr>),
    Block(Vec<Stmt>, Option<Box<Expr>>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    While(Box<Expr>, Box<Expr>),
    For(Box<Pattern>, Box<Expr>, Box<Expr>),
    Loop(Box<Expr>),
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
    Throw(Box<Expr>),
    Try(Box<Expr>, Vec<Catch>, Option<Box<Expr>>),
    TryExpr(Box<Expr>),
    Question(Box<Expr>),
    With(Box<Expr>, Option<usize>, Box<Expr>),
    Await(Box<Expr>),
    SetLocal(usize, Box<Expr>),
    SetIndex(Box<Expr>, Box<Expr>, Box<Expr>),
    SetPyIndex(Box<Expr>, Vec<PyIndexPart>, Box<Expr>),
    SetAttr(Box<Expr>, String, Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum PyArg {
    Pos(Expr),
    Named(String, Expr),
    Star(Expr),
    StarStar(Expr),
}

#[derive(Clone, Debug)]
pub enum PyIndexPart {
    Expr(Expr),
    Slice(Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>),
    Ellipsis,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Let(Pattern, Expr),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub local: usize,
    pub filters: Vec<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub ty: Ty,
    pub kind: PatKind,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    Bind(usize),
    Lit(Box<Expr>),
    Variant(DefId, Vec<(usize, Pattern)>),
    Tuple(Vec<Pattern>),
    List(Vec<Pattern>, bool),
    Or(Vec<Pattern>),
    TypeTest(usize),
    PyNone,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub owner: usize,
    pub cell: bool,
}

#[derive(Clone, Debug)]
pub struct Lambda {
    pub params: Vec<usize>,
    pub captures: Vec<usize>,
    pub body: Expr,
    pub ret: Ty,
}

#[derive(Clone, Debug)]
pub struct FnBody {
    pub def: DefId,
    pub params: Vec<usize>,
    pub locals: Vec<Local>,
    pub lambdas: Vec<Lambda>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Option<String>,
    pub ty: Ty,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub default: Option<Expr>,
    pub optional: bool,
    pub kind: ast::ParamKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FnKind {
    Native,
    Intrinsic,
    PyFunction,
    PyMethod,
    TraitMethod,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    pub generics: Vec<(String, Vec<Bound>)>,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub is_async: bool,
    pub kind: FnKind,
    pub owner: Option<DefId>,
}

#[derive(Clone, Debug)]
pub enum DefKind {
    Placeholder,
    Fn(FnSig),
    Struct { generics: Vec<String>, fields: Vec<Field> },
    Enum { generics: Vec<String>, variants: Vec<DefId> },
    Variant { parent: DefId, fields: Vec<Field> },
    Trait { methods: Vec<(String, DefId)>, generics: usize },
    PyClass { global: usize, fields: Vec<Field>, methods: Vec<(String, DefId)>, bases: Vec<Expr> },
    ExternType { generics: Vec<String>, attrs: Vec<(String, Ty)>, methods: Vec<(String, DefId)>, bases: Vec<Ty> },
    Alias(Vec<String>, Ty),
}

#[derive(Clone, Debug)]
pub struct Def {
    pub name: String,
    pub module: usize,
    pub export: bool,
    pub kind: DefKind,
}

#[derive(Clone, Debug)]
pub enum GlobalInit {
    Import(String),
    Attr(usize, String),
    Value(Expr),
    Class(DefId),
    Decorated(DefId, Vec<Expr>),
}

#[derive(Clone, Debug)]
pub struct Global {
    pub ty: Ty,
    pub init: GlobalInit,
    pub module: usize,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub struct Impl {
    pub trait_def: Option<DefId>,
    pub trait_args: Vec<Ty>,
    pub target: Ty,
    pub generics: usize,
    pub methods: Vec<(String, DefId)>,
}

pub struct Program {
    pub defs: Vec<Def>,
    pub fns: Vec<FnBody>,
    pub globals: Vec<Global>,
    pub impls: Vec<Impl>,
    pub subst: Vec<Option<Ty>>,
    pub entry_module: usize,
}

impl Program {
    pub fn resolve(&self, ty: &Ty) -> Ty {
        ty.map(&mut |ty| match ty {
            Ty::Var(var) => Some(match &self.subst[*var as usize] {
                Some(bound) => self.resolve(bound),
                None => Ty::Dyn,
            }),
            _ => None,
        })
    }

    pub fn sig(&self, def: DefId) -> &FnSig {
        match &self.defs[def].kind {
            DefKind::Fn(sig) => sig,
            _ => panic!("not a function: {}", self.defs[def].name),
        }
    }

    pub fn fields(&self, def: DefId) -> &[Field] {
        match &self.defs[def].kind {
            DefKind::Struct { fields, .. } | DefKind::Variant { fields, .. } | DefKind::PyClass { fields, .. } => fields,
            _ => &[],
        }
    }
}
