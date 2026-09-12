pub type Pos = u32;

#[derive(Clone, Debug)]
pub struct Module {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub py: bool,
    pub star: Option<String>,
    pub names: Vec<(String, String, Pos)>,
    pub from: String,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub decorators: Vec<Expr>,
    pub export: bool,
    pub kind: ItemKind,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Const(String, TypeExpr, Expr),
    TypeAlias(String, Vec<Generic>, TypeExpr),
    Extern(String, Vec<ExternItem>),
    PyClass(PyClassDecl),
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: String,
    pub pos: Pos,
    pub is_async: bool,
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    Normal,
    SelfParam,
    Star,
    StarStar,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub pos: Pos,
    pub kind: ParamKind,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub optional: bool,
    pub var: bool,
}

#[derive(Clone, Debug)]
pub struct Generic {
    pub name: String,
    pub bounds: Vec<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: String,
    pub generics: Vec<Generic>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: String,
    pub generics: Vec<Generic>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub pos: Pos,
    pub fields: Vec<(Option<String>, TypeExpr)>,
}

#[derive(Clone, Debug)]
pub struct TraitDecl {
    pub name: String,
    pub generics: Vec<Generic>,
    pub methods: Vec<FnDecl>,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub generics: Vec<Generic>,
    pub trait_name: Option<TypeExpr>,
    pub target: TypeExpr,
    pub methods: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum ExternItem {
    Fn(FnDecl),
    Const(String, TypeExpr),
    Type(ExternType),
}

#[derive(Clone, Debug)]
pub struct ExternType {
    pub name: String,
    pub generics: Vec<Generic>,
    pub bases: Vec<TypeExpr>,
    pub attrs: Vec<(String, TypeExpr)>,
    pub methods: Vec<FnDecl>,
}

#[derive(Clone, Debug)]
pub struct PyClassDecl {
    pub name: String,
    pub bases: Vec<Expr>,
    pub fields: Vec<Field>,
    pub methods: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum TypeExpr {
    Path(Vec<String>, Vec<TypeExpr>, Pos),
    Tuple(Vec<TypeExpr>, Pos),
    Fn(Vec<TypeExpr>, Box<TypeExpr>, Pos),
}

impl TypeExpr {
    pub fn pos(&self) -> Pos {
        match self {
            TypeExpr::Path(_, _, pos) | TypeExpr::Tuple(_, pos) | TypeExpr::Fn(_, _, pos) => *pos,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(Pattern, Option<TypeExpr>, Expr),
    Var(String, Option<TypeExpr>, Expr),
    Assign(Expr, Option<BinOp>, Expr, Pos),
    Expr(Expr),
    Item(Item),
    Try(Block, Vec<Catch>, Option<Block>, Pos),
    With(Vec<(Expr, Option<(String, Pos)>)>, Block, Pos),
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub name: String,
    pub filters: Vec<Expr>,
    pub body: Block,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, FloorDiv, Mod, Pow, MatMul,
    BitAnd, BitOr, BitXor, Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge, In,
    And, Or,
}

impl BinOp {
    pub fn text(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+", Sub => "-", Mul => "*", Div => "/", FloorDiv => "~/", Mod => "%", Pow => "**",
            MatMul => "@", BitAnd => "&", BitOr => "|", BitXor => "^", Shl => "<<", Shr => ">>",
            Eq => "==", Ne => "!=", Lt => "<", Le => "<=", Gt => ">", Ge => ">=", In => "in",
            And => "&&", Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CastKind {
    Static,
    Checked,
    Optional,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Unit,
    Template(Vec<TemplatePiece>),
    Name(String),
    SelfValue,
    Super,
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Block(Block),
    If(Box<Cond>, Block, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    While(Box<Cond>, Block),
    For(Box<Pattern>, Box<Expr>, Block),
    Loop(Block),
    Lambda(Vec<(String, Pos, Option<TypeExpr>)>, Box<Expr>),
    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,
    Throw(Box<Expr>),
    TryExpr(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Pipe(Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>, bool),
    Cast(Box<Expr>, CastKind, TypeExpr),
    Await(Box<Expr>),
    Call(Box<Expr>, Vec<TypeExpr>, Vec<Arg>),
    Index(Box<Expr>, Vec<IndexArg>),
    Member(Box<Expr>, String),
    TupleField(Box<Expr>, usize),
    Question(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum TemplatePiece {
    Text(String),
    Code(Expr),
}

#[derive(Clone, Debug)]
pub enum Arg {
    Pos(Expr),
    Named(String, Expr),
    Star(Expr),
    StarStar(Expr),
}

#[derive(Clone, Debug)]
pub enum IndexArg {
    Expr(Expr),
    Slice(Option<Expr>, Option<Expr>, Option<Expr>),
    Ellipsis,
}

#[derive(Clone, Debug)]
pub enum Cond {
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
pub struct Pattern {
    pub kind: PatKind,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    Lit(Box<Expr>),
    Ident(String),
    TypeTest(String, TypeExpr),
    Ctor(Vec<String>, Vec<SubPat>),
    Tuple(Vec<SubPat>),
    List(Vec<SubPat>),
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug)]
pub enum SubPat {
    Pat(Pattern),
    Rest,
    Named(String, Pattern),
}
