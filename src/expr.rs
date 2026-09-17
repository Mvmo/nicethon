use crate::ast::{self, BinOp, CastKind, Pos, UnOp};
use crate::check::{same_head, Binding, Checker};
use crate::hir::*;
use std::collections::HashMap;

fn node(ty: Ty, kind: ExprKind, pos: Pos) -> Expr {
    Expr { ty, kind, pos }
}

fn boxed(expr: Expr) -> Box<Expr> {
    Box::new(expr)
}

enum Constructor {
    Variant(DefId),
    Bool(bool),
    Tuple,
}

enum Callee {
    Native(DefId, Option<Expr>),
    Construct(DefId),
    Value(Expr),
    Py(Expr, Option<DefId>, Option<Ty>),
    PyConstruct(Expr, DefId),
    Trait(Expr, DefId, Vec<Ty>),
    Copy(Expr, DefId),
}

impl Checker {
    // Locals:

    pub fn declare_local(&mut self, name: &str, ty: Ty, mutable: bool) -> usize {
        let owner = *self.f.owners.last().unwrap();
        self.f.locals.push(Local { name: name.to_string(), ty, mutable, owner, cell: false });
        let id = self.f.locals.len() - 1;
        self.f.scopes.last_mut().unwrap().insert(name.to_string(), id);
        id
    }

    fn find_local(&mut self, name: &str) -> Option<usize> {
        let id = self.f.scopes.iter().rev().find_map(|scope| scope.get(name).copied())?;
        let state = &mut self.f;
        let owner = state.locals[id].owner;
        let start = state.owners.iter().position(|o| *o == owner).unwrap_or(0);
        for &lambda_owner in &state.owners[start + 1..] {
            let captures = &mut state.lambdas[lambda_owner - 1].captures;
            if !captures.contains(&id) {
                captures.push(id);
            }
            if state.locals[id].mutable {
                state.locals[id].cell = true;
            }
        }
        Some(id)
    }

    fn scoped<T>(&mut self, body: impl FnOnce(&mut Self) -> T) -> T {
        self.f.scopes.push(HashMap::new());
        let result = body(self);
        self.f.scopes.pop();
        result
    }

    // Coercion:

    pub fn coerce(&mut self, value: Expr, expected: &Ty) -> Expr {
        let actual = self.shallow(&value.ty);
        if self.shallow(expected) == Ty::Dyn && !matches!(actual, Ty::Dyn | Ty::Var(_) | Ty::Error | Ty::Never) {
            return self.dynamic(value);
        }
        if !self.unify(expected, &value.ty) {
            let message = format!("expected {}, found {}", self.show(expected), self.show(&value.ty));
            self.error(value.pos, message);
        }
        value
    }

    pub fn dynamic(&mut self, value: Expr) -> Expr {
        if matches!(self.shallow(&value.ty), Ty::Dyn | Ty::Error) {
            return value;
        }
        let pos = value.pos;
        node(Ty::Dyn, ExprKind::ToDyn(boxed(value)), pos)
    }

    fn typed(&mut self, value: Expr, ty: Ty) -> Expr {
        let pos = value.pos;
        match self.shallow(&ty) {
            Ty::Dyn => value,
            Ty::Unit => Expr { ty: Ty::Unit, ..value },
            _ => node(ty, ExprKind::FromDyn(boxed(value), false), pos),
        }
    }

    fn join(&mut self, left: &Ty, right: &Ty, pos: Pos) -> Ty {
        match (self.shallow(left), self.shallow(right)) {
            (Ty::Never, _) => right.clone(),
            (_, Ty::Never) => left.clone(),
            _ => {
                if !self.unify(left, right) {
                    let message = format!("branches have different types: {} and {}", self.show(left), self.show(right));
                    self.error(pos, message);
                }
                left.clone()
            }
        }
    }

    fn branch(&mut self, value: Expr, expected: Option<&Ty>, result: &mut Option<Ty>) -> Expr {
        match expected {
            Some(expected) if !matches!(self.shallow(expected), Ty::Var(_) | Ty::Unit) => {
                *result = Some(expected.clone());
                self.coerce(value, expected)
            }
            _ => {
                *result = Some(match result.take() {
                    Some(ty) => self.join(&ty, &value.ty, value.pos),
                    None => value.ty.clone(),
                });
                value
            }
        }
    }

    // Statements:

    pub fn check_block(&mut self, block: &ast::Block, expected: Option<&Ty>) -> Expr {
        self.scoped(|this| {
            let mut stmts = vec![];
            for stmt in &block.stmts {
                if let Some(stmt) = this.check_stmt(stmt) {
                    stmts.push(stmt);
                }
            }
            let tail = block.tail.as_ref().map(|tail| boxed(this.check_expr(tail, expected)));
            let ty = match (&tail, stmts.last()) {
                (Some(tail), _) => tail.ty.clone(),
                (None, Some(Stmt::Expr(last))) if this.shallow(&last.ty) == Ty::Never => Ty::Never,
                _ => Ty::Unit,
            };
            node(ty, ExprKind::Block(stmts, tail), block.pos)
        })
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt) -> Option<Stmt> {
        match stmt {
            ast::Stmt::Let(pattern, ty, value) => {
                let declared = ty.as_ref().map(|ty| self.resolve_here(ty));
                let value = self.check_expr(value, declared.as_ref());
                let value = match &declared {
                    Some(ty) => self.coerce(value, ty),
                    None => value,
                };
                let ty = declared.unwrap_or_else(|| value.ty.clone());
                let pattern = self.check_pattern(pattern, &ty);
                if !self.irrefutable(&pattern) {
                    self.error(pattern_pos(stmt), "let patterns must always match, use if let or match");
                }
                Some(Stmt::Let(pattern, value))
            }
            ast::Stmt::Var(name, ty, value) => {
                let declared = ty.as_ref().map(|ty| self.resolve_here(ty));
                let value = self.check_expr(value, declared.as_ref());
                let value = match &declared {
                    Some(ty) => self.coerce(value, ty),
                    None => value,
                };
                let ty = declared.unwrap_or_else(|| value.ty.clone());
                let local = self.declare_local(name, ty.clone(), true);
                Some(Stmt::Let(Pattern { ty, kind: PatKind::Bind(local) }, value))
            }
            ast::Stmt::Assign(place, op, value, pos) => Some(Stmt::Expr(self.check_assign(place, *op, value, *pos))),
            ast::Stmt::Expr(expr) => Some(Stmt::Expr(self.check_expr(expr, None))),
            ast::Stmt::Item(item) => {
                let ast::ItemKind::Fn(decl) = &item.kind else {
                    self.error(item.pos, "only functions can be nested inside functions");
                    return None;
                };
                let params = decl.params.iter().map(|p| (p.name.clone(), p.pos, p.ty.clone())).collect::<Vec<_>>();
                let ret = decl.ret.as_ref().map_or(Ty::Unit, |ty| self.resolve_here(ty));
                let body = ast::Expr { kind: ast::ExprKind::Block(decl.body.clone()?), pos: decl.pos };
                let param_tys: Vec<Ty> = params.iter().map(|(_, _, ty)| ty.as_ref().map_or(Ty::Error, |ty| self.resolve_here(ty))).collect();
                let lambda = self.check_lambda(&params, &body, Some(&Ty::Fn(param_tys, Box::new(ret))), decl.pos);
                let ty = lambda.ty.clone();
                let local = self.declare_local(&decl.name, ty.clone(), false);
                Some(Stmt::Let(Pattern { ty, kind: PatKind::Bind(local) }, lambda))
            }
            ast::Stmt::Try(body, catches, finally, pos) => {
                let body = self.check_block(body, None);
                let catches: Vec<Catch> = catches
                    .iter()
                    .map(|catch| {
                        self.scoped(|this| {
                            let filters = catch.filters.iter().map(|f| {
                                let filter = this.check_expr(f, None);
                                this.dynamic(filter)
                            }).collect();
                            let local = this.declare_local(&catch.name, this.builtin_ty("PyErr", vec![]), false);
                            Catch { local, filters, body: this.check_block(&catch.body, None) }
                        })
                    })
                    .collect();
                let finally = finally.as_ref().map(|block| boxed(self.check_block(block, None)));
                let diverges = self.shallow(&body.ty) == Ty::Never && catches.iter().all(|c| self.shallow(&c.body.ty) == Ty::Never);
                let ty = if diverges { Ty::Never } else { Ty::Unit };
                Some(Stmt::Expr(node(ty, ExprKind::Try(boxed(body), catches, finally), *pos)))
            }
            ast::Stmt::With(items, body, pos) => Some(Stmt::Expr(self.scoped(|this| this.check_with(items, body, *pos)))),
        }
    }

    fn check_with(&mut self, items: &[(ast::Expr, Option<(String, Pos)>)], body: &ast::Block, pos: Pos) -> Expr {
        let Some(((manager, name), rest)) = items.split_first() else {
            return self.check_block(body, None);
        };
        let manager = self.check_expr(manager, None);
        let entered = match self.shallow(&manager.ty) {
            Ty::Named(def, args) => self.find_py_method(def, "__enter__").map(|m| self.sig(m).ret.subst(&args)).unwrap_or(Ty::Dyn),
            _ => Ty::Dyn,
        };
        let manager = self.dynamic(manager);
        let local = name.as_ref().map(|(name, _)| self.declare_local(name, entered, false));
        let inner = self.check_with(rest, body, pos);
        let ty = if self.shallow(&inner.ty) == Ty::Never { Ty::Never } else { Ty::Unit };
        node(ty, ExprKind::With(boxed(manager), local, boxed(inner)), pos)
    }

    fn check_assign(&mut self, place: &ast::Expr, op: Option<BinOp>, value: &ast::Expr, pos: Pos) -> Expr {
        let combined;
        let value = match op {
            Some(op) => {
                combined = ast::Expr { kind: ast::ExprKind::Binary(op, Box::new(place.clone()), Box::new(value.clone())), pos };
                &combined
            }
            None => value,
        };
        match &place.kind {
            ast::ExprKind::Name(name) => {
                let Some(local) = self.find_local(name) else {
                    self.error(place.pos, format!("cannot assign to {name}"));
                    return node(Ty::Unit, ExprKind::Unit, pos);
                };
                if !self.f.locals[local].mutable {
                    self.error(place.pos, format!("cannot assign to {name}, it is declared with let"));
                }
                let ty = self.f.locals[local].ty.clone();
                let value = if let (Some(op), Ty::Dyn) = (op, self.shallow(&ty)) {
                    let ast::ExprKind::Binary(_, _, right) = &value.kind else { unreachable!() };
                    let right = self.check_expr(right, None);
                    let right = self.dynamic(right);
                    node(Ty::Dyn, ExprKind::Inplace(op, boxed(node(Ty::Dyn, ExprKind::Local(local), pos)), boxed(right)), pos)
                } else {
                    let value = self.check_expr(value, Some(&ty));
                    self.coerce(value, &ty)
                };
                node(Ty::Unit, ExprKind::SetLocal(local, boxed(value)), pos)
            }
            ast::ExprKind::Member(object, field) => {
                let target = self.check_expr(object, None);
                match self.shallow(&target.ty) {
                    Ty::Named(def, args) if matches!(self.defs[def].kind, DefKind::Struct { .. }) => {
                        let Some(index) = self.fields_of(def).iter().position(|f| f.name.as_deref() == Some(field)) else {
                            self.error(place.pos, format!("{} has no field {field}", self.defs[def].name));
                            return node(Ty::Unit, ExprKind::Unit, pos);
                        };
                        let field_ty = self.fields_of(def)[index].ty.subst(&args);
                        let value = self.check_expr(value, Some(&field_ty));
                        let value = self.coerce(value, &field_ty);
                        let ty = target.ty.clone();
                        let updated = node(ty, ExprKind::CopyWith(boxed(target), def, vec![(index, value)]), pos);
                        self.assign_value(object, updated, &path_text(place), pos)
                    }
                    Ty::Named(def, args) if matches!(self.defs[def].kind, DefKind::PyClass { .. } | DefKind::ExternType { .. }) => {
                        let field_ty = self.py_attr_type(def, &args, field).unwrap_or(Ty::Dyn);
                        let value = self.check_expr(value, Some(&field_ty));
                        let value = self.coerce(value, &field_ty);
                        let value = self.dynamic(value);
                        node(Ty::Unit, ExprKind::SetAttr(boxed(target), field.clone(), boxed(value)), pos)
                    }
                    Ty::Dyn | Ty::Error => {
                        let value = self.check_expr(value, None);
                        let value = self.dynamic(value);
                        node(Ty::Unit, ExprKind::SetAttr(boxed(target), field.clone(), boxed(value)), pos)
                    }
                    other => {
                        let message = format!("{} has no field {field}", self.show(&other));
                        self.error(place.pos, message);
                        node(Ty::Unit, ExprKind::Unit, pos)
                    }
                }
            }
            ast::ExprKind::Index(object, args) => {
                let target = self.check_expr(object, None);
                match (self.shallow(&target.ty), args.as_slice()) {
                    (Ty::List(item), [ast::IndexArg::Expr(index)]) => {
                        let index = self.check_expr(index, Some(&Ty::Int));
                        let index = self.coerce(index, &Ty::Int);
                        let value = self.check_expr(value, Some(&item));
                        let value = self.coerce(value, &item);
                        node(Ty::Unit, ExprKind::SetIndex(boxed(target), boxed(index), boxed(value)), pos)
                    }
                    (Ty::Map(key, item), [ast::IndexArg::Expr(index)]) => {
                        let index = self.check_expr(index, Some(&key));
                        let index = self.coerce(index, &key);
                        let value = self.check_expr(value, Some(&item));
                        let value = self.coerce(value, &item);
                        node(Ty::Unit, ExprKind::SetIndex(boxed(target), boxed(index), boxed(value)), pos)
                    }
                    (Ty::List(_) | Ty::Map(..), _) => {
                        self.error(place.pos, "expected a single index");
                        node(Ty::Unit, ExprKind::Unit, pos)
                    }
                    _ => {
                        let target = self.dynamic(target);
                        let parts = self.index_parts(args);
                        let value = if let Some(op) = op {
                            let ast::ExprKind::Binary(_, _, right) = &value.kind else { unreachable!() };
                            let right = self.check_expr(right, None);
                            let right = self.dynamic(right);
                            let current = node(Ty::Dyn, ExprKind::PyIndex(boxed(target.clone()), parts.clone()), pos);
                            node(Ty::Dyn, ExprKind::Inplace(op, boxed(current), boxed(right)), pos)
                        } else {
                            let value = self.check_expr(value, None);
                            self.dynamic(value)
                        };
                        node(Ty::Unit, ExprKind::SetPyIndex(boxed(target), parts, boxed(value)), pos)
                    }
                }
            }
            _ => {
                self.error(place.pos, "cannot assign to this expression");
                node(Ty::Unit, ExprKind::Unit, pos)
            }
        }
    }

    fn assign_value(&mut self, place: &ast::Expr, value: Expr, path: &str, pos: Pos) -> Expr {
        match &place.kind {
            ast::ExprKind::Name(name) => {
                let Some(local) = self.find_local(name) else {
                    self.error(place.pos, format!("cannot assign to {path}"));
                    return node(Ty::Unit, ExprKind::Unit, pos);
                };
                if !self.f.locals[local].mutable {
                    self.error(place.pos, format!("cannot assign to {path}, {name} is declared with let"));
                }
                node(Ty::Unit, ExprKind::SetLocal(local, boxed(value)), pos)
            }
            ast::ExprKind::Member(object, field) => {
                let target = self.check_expr(object, None);
                let Ty::Named(def, _) = self.shallow(&target.ty) else {
                    self.error(place.pos, format!("cannot assign to {path}"));
                    return node(Ty::Unit, ExprKind::Unit, pos);
                };
                let Some(index) = self.fields_of(def).iter().position(|f| f.name.as_deref() == Some(field)) else {
                    self.error(place.pos, format!("cannot assign to {path}"));
                    return node(Ty::Unit, ExprKind::Unit, pos);
                };
                let ty = target.ty.clone();
                let updated = node(ty, ExprKind::CopyWith(boxed(target), def, vec![(index, value)]), pos);
                self.assign_value(object, updated, path, pos)
            }
            _ => {
                self.error(place.pos, format!("cannot assign to {path}"));
                node(Ty::Unit, ExprKind::Unit, pos)
            }
        }
    }

    // Expressions:

    pub fn check_expr(&mut self, expr: &ast::Expr, expected: Option<&Ty>) -> Expr {
        let pos = expr.pos;
        let expected_shallow = expected.map(|ty| self.shallow(ty));
        match &expr.kind {
            ast::ExprKind::Int(value) => self.int_literal(*value, expected_shallow, pos),
            ast::ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ast::ExprKind::Int(_)) => {
                let ast::ExprKind::Int(value) = inner.kind else { unreachable!() };
                self.int_literal(-value, expected_shallow, pos)
            }
            ast::ExprKind::Float(value) => node(Ty::Float, ExprKind::Float(*value), pos),
            ast::ExprKind::Str(text) => node(Ty::Str, ExprKind::Str(text.clone()), pos),
            ast::ExprKind::Bytes(bytes) => node(Ty::Bytes, ExprKind::Bytes(bytes.clone()), pos),
            ast::ExprKind::Bool(value) => node(Ty::Bool, ExprKind::Bool(*value), pos),
            ast::ExprKind::Unit => node(Ty::Unit, ExprKind::Unit, pos),
            ast::ExprKind::Template(pieces) => {
                let parts = pieces
                    .iter()
                    .map(|piece| match piece {
                        ast::TemplatePiece::Text(text) => node(Ty::Str, ExprKind::Str(text.clone()), pos),
                        ast::TemplatePiece::Code(code) => self.check_expr(code, None),
                    })
                    .collect();
                node(Ty::Str, ExprKind::Template(parts), pos)
            }
            ast::ExprKind::Name(name) => self.check_name(name, pos),
            ast::ExprKind::SelfValue => match self.find_local("self") {
                Some(local) => node(self.f.locals[local].ty.clone(), ExprKind::Local(local), pos),
                None => {
                    self.error(pos, "self is only available in methods");
                    node(Ty::Error, ExprKind::Unit, pos)
                }
            },
            ast::ExprKind::Super => self.super_value(pos),
            ast::ExprKind::Tuple(items) => {
                let hints = match &expected_shallow {
                    Some(Ty::Tuple(hints)) if hints.len() == items.len() => hints.clone(),
                    _ => vec![],
                };
                let items: Vec<Expr> = items.iter().enumerate().map(|(i, item)| self.check_expr(item, hints.get(i))).collect();
                node(Ty::Tuple(items.iter().map(|i| i.ty.clone()).collect()), ExprKind::Tuple(items), pos)
            }
            ast::ExprKind::List(items) => {
                let item_ty = match &expected_shallow {
                    Some(Ty::List(item)) => (**item).clone(),
                    _ => self.fresh(),
                };
                let items = items.iter().map(|item| {
                    let value = self.check_expr(item, Some(&item_ty));
                    self.coerce(value, &item_ty)
                }).collect();
                node(Ty::List(Box::new(item_ty)), ExprKind::List(items), pos)
            }
            ast::ExprKind::Map(entries) => {
                let (key_ty, value_ty) = match &expected_shallow {
                    Some(Ty::Map(key, value)) => ((**key).clone(), (**value).clone()),
                    _ => (self.fresh(), self.fresh()),
                };
                let entries = entries.iter().map(|(key, value)| {
                    let key = self.check_expr(key, Some(&key_ty));
                    let key = self.coerce(key, &key_ty);
                    let value = self.check_expr(value, Some(&value_ty));
                    (key, self.coerce(value, &value_ty))
                }).collect();
                node(Ty::Map(Box::new(key_ty), Box::new(value_ty)), ExprKind::Map(entries), pos)
            }
            ast::ExprKind::Block(block) => self.check_block(block, expected),
            ast::ExprKind::If(cond, then, otherwise) => match &**cond {
                ast::Cond::Let(pattern, value) => {
                    let arms = vec![
                        ast::Arm { pattern: pattern.clone(), guard: None, body: ast::Expr { kind: ast::ExprKind::Block(then.clone()), pos } },
                        ast::Arm {
                            pattern: ast::Pattern { kind: ast::PatKind::Wild, pos },
                            guard: None,
                            body: otherwise.as_deref().cloned().unwrap_or(ast::Expr { kind: ast::ExprKind::Unit, pos }),
                        },
                    ];
                    let expected = if otherwise.is_some() { expected } else { None };
                    let mut result = self.check_match(value, &arms, expected, pos);
                    if otherwise.is_none() {
                        result.ty = Ty::Unit;
                    }
                    result
                }
                ast::Cond::Expr(cond) => {
                    let cond = self.check_cond(cond);
                    match otherwise {
                        None => {
                            let then = self.check_block(then, None);
                            node(Ty::Unit, ExprKind::If(boxed(cond), boxed(then), None), pos)
                        }
                        Some(otherwise) => {
                            let mut result = None;
                            let then = self.check_block(then, expected);
                            let then = self.branch(then, expected, &mut result);
                            let otherwise = self.check_expr(otherwise, expected);
                            let otherwise = self.branch(otherwise, expected, &mut result);
                            node(result.unwrap(), ExprKind::If(boxed(cond), boxed(then), Some(boxed(otherwise))), pos)
                        }
                    }
                }
            },
            ast::ExprKind::Match(scrutinee, arms) => self.check_match(scrutinee, arms, expected, pos),
            ast::ExprKind::While(cond, body) => match &**cond {
                ast::Cond::Let(pattern, value) => {
                    let arms = vec![
                        ast::Arm { pattern: pattern.clone(), guard: None, body: ast::Expr { kind: ast::ExprKind::Block(body.clone()), pos } },
                        ast::Arm { pattern: ast::Pattern { kind: ast::PatKind::Wild, pos }, guard: None, body: ast::Expr { kind: ast::ExprKind::Break(None), pos } },
                    ];
                    let matcher = ast::Expr { kind: ast::ExprKind::Match(Box::new(value.clone()), arms), pos };
                    let block = ast::Block { stmts: vec![ast::Stmt::Expr(matcher)], tail: None, pos };
                    self.f.loops.push(None);
                    let body = self.check_block(&block, None);
                    self.f.loops.pop();
                    node(Ty::Unit, ExprKind::Loop(boxed(body)), pos)
                }
                ast::Cond::Expr(cond) => {
                    let cond = self.check_cond(cond);
                    self.f.loops.push(None);
                    let body = self.check_block(body, None);
                    self.f.loops.pop();
                    node(Ty::Unit, ExprKind::While(boxed(cond), boxed(body)), pos)
                }
            },
            ast::ExprKind::For(pattern, iterable, body) => self.check_for(pattern, iterable, body, pos),
            ast::ExprKind::Loop(body) => {
                let break_ty = self.fresh();
                self.f.loops.push(Some(break_ty.clone()));
                let body = self.check_block(body, None);
                self.f.loops.pop();
                let ty = match self.shallow(&break_ty) {
                    Ty::Var(_) => Ty::Never,
                    ty => ty,
                };
                node(ty, ExprKind::Loop(boxed(body)), pos)
            }
            ast::ExprKind::Lambda(params, body) => self.check_lambda(params, body, expected, pos),
            ast::ExprKind::Return(value) => {
                let ret = self.f.rets.last().cloned().unwrap_or(Ty::Unit);
                let value = value.as_ref().map(|value| {
                    let value = self.check_expr(value, Some(&ret));
                    boxed(self.coerce(value, &ret))
                });
                if value.is_none() && !matches!(self.shallow(&ret), Ty::Unit | Ty::Var(_)) {
                    self.error(pos, "expected a return value");
                }
                node(Ty::Never, ExprKind::Return(value), pos)
            }
            ast::ExprKind::Break(value) => {
                let Some(target) = self.f.loops.last().cloned() else {
                    self.error(pos, "break outside of a loop");
                    return node(Ty::Never, ExprKind::Unit, pos);
                };
                let value = match (value, target) {
                    (Some(value), Some(ty)) => {
                        let value = self.check_expr(value, Some(&ty));
                        Some(boxed(self.coerce(value, &ty)))
                    }
                    (Some(value), None) => {
                        self.error(value.pos, "break with a value is only allowed in loop");
                        None
                    }
                    (None, Some(ty)) => {
                        self.unify(&ty, &Ty::Unit);
                        None
                    }
                    (None, None) => None,
                };
                node(Ty::Never, ExprKind::Break(value), pos)
            }
            ast::ExprKind::Continue => {
                if self.f.loops.is_empty() {
                    self.error(pos, "continue outside of a loop");
                }
                node(Ty::Never, ExprKind::Continue, pos)
            }
            ast::ExprKind::Throw(value) => {
                let value = self.check_expr(value, None);
                let throwable = match self.shallow(&value.ty) {
                    Ty::Dyn | Ty::Error => true,
                    Ty::Named(def, _) => Some(&def) == self.builtins.get("PyErr") || matches!(self.defs[def].kind, DefKind::ExternType { .. } | DefKind::PyClass { .. }),
                    _ => false,
                };
                if !throwable {
                    let message = format!("cannot throw {}, only python exceptions", self.show(&value.ty));
                    self.error(value.pos, message);
                }
                node(Ty::Never, ExprKind::Throw(boxed(value)), pos)
            }
            ast::ExprKind::TryExpr(inner) => {
                let inner = self.check_expr(inner, None);
                let ty = self.builtin_ty("Result", vec![inner.ty.clone(), self.builtin_ty("PyErr", vec![])]);
                node(ty, ExprKind::TryExpr(boxed(inner)), pos)
            }
            ast::ExprKind::Binary(op, left, right) => self.check_binary(*op, left, right, pos, expected),
            ast::ExprKind::Unary(op, inner) => self.check_unary(*op, inner, pos, expected),
            ast::ExprKind::Pipe(left, right) => {
                let call = match &right.kind {
                    ast::ExprKind::Call(callee, type_args, args) => {
                        let mut all = vec![ast::Arg::Pos((**left).clone())];
                        all.extend(args.iter().cloned());
                        ast::ExprKind::Call(callee.clone(), type_args.clone(), all)
                    }
                    _ => ast::ExprKind::Call(right.clone(), vec![], vec![ast::Arg::Pos((**left).clone())]),
                };
                self.check_expr(&ast::Expr { kind: call, pos }, expected)
            }
            ast::ExprKind::Range(start, end, inclusive) => {
                let start = self.check_expr(start, Some(&Ty::Int));
                let start = self.coerce(start, &Ty::Int);
                let end = self.check_expr(end, Some(&Ty::Int));
                let end = self.coerce(end, &Ty::Int);
                node(Ty::List(Box::new(Ty::Int)), ExprKind::Range(boxed(start), boxed(end), *inclusive), pos)
            }
            ast::ExprKind::Cast(inner, kind, ty) => {
                let target = self.resolve_here(ty);
                let inner = self.check_expr(inner, None);
                let source = self.shallow(&inner.ty);
                match kind {
                    CastKind::Static if self.has_vars(&inner.ty) && self.compatible(&inner.ty, &target) => {
                        self.unify(&inner.ty, &target);
                        inner
                    }
                    CastKind::Static => match (&source, self.shallow(&target)) {
                        (Ty::Error, _) => inner,
                        (_, Ty::Dyn) => self.dynamic(inner),
                        (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) | (Ty::Int, Ty::BigInt) | (Ty::BigInt, Ty::Int) | (Ty::BigInt, Ty::Float) => {
                            node(target, ExprKind::Cast(boxed(inner)), pos)
                        }
                        (source, target) if *source == target => inner,
                        _ => {
                            let message = format!("cannot convert {} to {} with as", self.show(&source), self.show(&target));
                            self.error(pos, message);
                            inner
                        }
                    },
                    CastKind::Checked | CastKind::Optional => {
                        if !matches!(source, Ty::Dyn | Ty::Error) {
                            let keyword = if *kind == CastKind::Checked { "as!" } else { "as?" };
                            self.error(pos, format!("{keyword} needs a Dyn operand, found {}", self.show(&source)));
                        }
                        let optional = *kind == CastKind::Optional;
                        let ty = if optional { self.builtin_ty("Option", vec![target]) } else { target };
                        node(ty, ExprKind::FromDyn(boxed(inner), optional), pos)
                    }
                }
            }
            ast::ExprKind::Await(inner) => {
                if !self.f.is_async {
                    self.error(pos, "await is only allowed in async functions");
                }
                let inner = self.check_expr(inner, None);
                let ty = match self.shallow(&inner.ty) {
                    Ty::Coroutine(result) => *result,
                    Ty::Dyn | Ty::Error => Ty::Dyn,
                    other => {
                        let message = format!("cannot await {}", self.show(&other));
                        self.error(pos, message);
                        Ty::Error
                    }
                };
                let inner = self.dynamic(inner);
                node(ty, ExprKind::Await(boxed(inner)), pos)
            }
            ast::ExprKind::Call(callee, _, args) => self.check_call(callee, args, pos, expected),
            ast::ExprKind::Index(object, args) => {
                let target = self.check_expr(object, None);
                match (self.shallow(&target.ty), args.as_slice()) {
                    (Ty::List(item), [ast::IndexArg::Expr(index)]) => {
                        let index = self.check_expr(index, Some(&Ty::Int));
                        let index = self.coerce(index, &Ty::Int);
                        node(*item, ExprKind::Index(boxed(target), boxed(index)), pos)
                    }
                    (Ty::Map(key, item), [ast::IndexArg::Expr(index)]) => {
                        let index = self.check_expr(index, Some(&key));
                        let index = self.coerce(index, &key);
                        node(*item, ExprKind::Index(boxed(target), boxed(index)), pos)
                    }
                    (Ty::Dyn | Ty::Error, _) | (Ty::Named(..), _) => {
                        let target = self.dynamic(target);
                        let parts = self.index_parts(args);
                        node(Ty::Dyn, ExprKind::PyIndex(boxed(target), parts), pos)
                    }
                    (other, _) => {
                        let message = format!("cannot index {}", self.show(&other));
                        self.error(pos, message);
                        node(Ty::Error, ExprKind::Unit, pos)
                    }
                }
            }
            ast::ExprKind::Member(object, name) => self.check_member(object, name, pos),
            ast::ExprKind::TupleField(object, index) => {
                let target = self.check_expr(object, None);
                match self.shallow(&target.ty) {
                    Ty::Tuple(items) if *index < items.len() => node(items[*index].clone(), ExprKind::TupleGet(boxed(target), *index), pos),
                    other => {
                        let message = format!("{} has no field {index}", self.show(&other));
                        self.error(pos, message);
                        node(Ty::Error, ExprKind::Unit, pos)
                    }
                }
            }
            ast::ExprKind::Question(inner) => self.check_question(inner, pos),
        }
    }

    fn int_literal(&mut self, value: i64, expected: Option<Ty>, pos: Pos) -> Expr {
        match expected {
            Some(Ty::Float) => node(Ty::Float, ExprKind::Float(value as f64), pos),
            Some(Ty::BigInt) => node(Ty::BigInt, ExprKind::Int(value), pos),
            _ => node(Ty::Int, ExprKind::Int(value), pos),
        }
    }

    fn check_cond(&mut self, cond: &ast::Expr) -> Expr {
        let cond = self.check_expr(cond, Some(&Ty::Bool));
        if !matches!(self.shallow(&cond.ty), Ty::Bool | Ty::Error | Ty::Never) && !self.unify(&cond.ty, &Ty::Bool) {
            let message = format!("condition must be Bool, found {}", self.show(&cond.ty));
            self.error(cond.pos, message);
        }
        cond
    }

    fn super_value(&mut self, pos: Pos) -> Expr {
        let (Some(class), Some(this)) = (self.f.class, self.find_local("self")) else {
            self.error(pos, "super is only available in py class methods");
            return node(Ty::Error, ExprKind::Unit, pos);
        };
        let DefKind::PyClass { global, .. } = self.defs[class].kind else { unreachable!() };
        node(Ty::Dyn, ExprKind::Super(global, this), pos)
    }

    fn check_question(&mut self, inner: &ast::Expr, pos: Pos) -> Expr {
        let inner = self.check_expr(inner, None);
        let ret = self.f.rets.last().cloned().unwrap_or(Ty::Unit);
        let (option, result) = (self.builtins["Option"], self.builtins["Result"]);
        let ret_head = match self.shallow(&ret) {
            Ty::Named(def, args) if def == option || def == result => Some((def, args)),
            _ => None,
        };
        let value_ty = match (self.shallow(&inner.ty), ret_head) {
            (Ty::Error, _) => Ty::Error,
            (_, None) => {
                self.error(pos, "? needs the enclosing function to return Result or Option");
                Ty::Error
            }
            (Ty::Named(def, args), Some((ret_def, ret_args))) if def == result && ret_def == result => {
                if !self.unify(&args[1], &ret_args[1]) {
                    let message = format!("? cannot return error {} from a function returning {}", self.show(&args[1]), self.show(&ret));
                    self.error(pos, message);
                }
                args[0].clone()
            }
            (Ty::Named(def, args), Some((ret_def, _))) if def == option && ret_def == option => args[0].clone(),
            (other, _) => {
                let message = format!("? needs a Result or Option matching the function, found {}", self.show(&other));
                self.error(pos, message);
                Ty::Error
            }
        };
        node(value_ty, ExprKind::Question(boxed(inner)), pos)
    }

    fn check_for(&mut self, pattern: &ast::Pattern, iterable: &ast::Expr, body: &ast::Block, pos: Pos) -> Expr {
        let iterable = self.check_expr(iterable, None);
        let item = match self.shallow(&iterable.ty) {
            Ty::List(item) | Ty::Set(item) => *item,
            Ty::Map(key, _) => *key,
            Ty::Str => Ty::Str,
            Ty::Dyn | Ty::Error => Ty::Dyn,
            Ty::Named(def, _) if matches!(self.defs[def].kind, DefKind::ExternType { .. } | DefKind::PyClass { .. }) => Ty::Dyn,
            other => {
                let message = format!("cannot iterate over {}", self.show(&other));
                self.error(iterable.pos, message);
                Ty::Error
            }
        };
        let iterable = if matches!(iterable.ty, Ty::Named(..)) { self.dynamic(iterable) } else { iterable };
        self.scoped(|this| {
            let pattern = this.check_pattern(pattern, &item);
            if !this.irrefutable(&pattern) {
                this.error(pos, "for patterns must always match");
            }
            this.f.loops.push(None);
            let body = this.check_block(body, None);
            this.f.loops.pop();
            node(Ty::Unit, ExprKind::For(Box::new(pattern), boxed(iterable), boxed(body)), pos)
        })
    }

    pub fn check_lambda(&mut self, params: &[(String, Pos, Option<ast::TypeExpr>)], body: &ast::Expr, expected: Option<&Ty>, pos: Pos) -> Expr {
        let expected = expected.map(|ty| self.shallow(ty));
        let (hint_params, hint_ret) = match &expected {
            Some(Ty::Fn(params_hint, ret)) if params_hint.len() == params.len() => (Some(params_hint.clone()), Some((**ret).clone())),
            _ => (None, None),
        };
        let dynamic = expected == Some(Ty::Dyn);
        let index = self.f.lambdas.len();
        self.f.lambdas.push(Lambda { params: vec![], captures: vec![], body: node(Ty::Unit, ExprKind::Unit, pos), ret: Ty::Unit });
        self.f.owners.push(index + 1);
        self.f.scopes.push(HashMap::new());
        let mut param_tys = vec![];
        let mut locals = vec![];
        for (i, (name, param_pos, ty)) in params.iter().enumerate() {
            let hint = hint_params.as_ref().map(|h| h[i].clone());
            let ty = match (ty, hint) {
                (Some(ty), hint) => {
                    let ty = self.resolve_here(ty);
                    if let Some(hint) = hint {
                        if !self.unify(&ty, &hint) {
                            let message = format!("expected parameter of type {}, found {}", self.show(&hint), self.show(&ty));
                            self.error(*param_pos, message);
                        }
                    }
                    ty
                }
                (None, Some(hint)) if !matches!(self.shallow(&hint), Ty::Error) => hint,
                (None, _) if dynamic => Ty::Dyn,
                (None, _) => {
                    let var = self.fresh();
                    self.f.infer.push((var.clone(), *param_pos, name.clone()));
                    var
                }
            };
            param_tys.push(ty.clone());
            locals.push(self.declare_local(name, ty, false));
        }
        let ret = hint_ret.unwrap_or_else(|| self.fresh());
        self.f.rets.push(ret.clone());
        let saved_loops = std::mem::take(&mut self.f.loops);
        let saved_async = std::mem::replace(&mut self.f.is_async, false);
        let value = self.check_expr(body, Some(&ret));
        self.f.is_async = saved_async;
        let value = if self.shallow(&ret) == Ty::Unit { value } else { self.coerce(value, &ret) };
        self.f.loops = saved_loops;
        self.f.rets.pop();
        self.f.scopes.pop();
        self.f.owners.pop();
        let lambda = &mut self.f.lambdas[index];
        lambda.params = locals;
        lambda.body = value;
        lambda.ret = ret.clone();
        node(Ty::Fn(param_tys, Box::new(ret)), ExprKind::Lambda(index), pos)
    }

    fn check_binary(&mut self, op: BinOp, left: &ast::Expr, right: &ast::Expr, pos: Pos, expected: Option<&Ty>) -> Expr {
        if matches!(op, BinOp::And | BinOp::Or) {
            let left = self.check_cond(left);
            let right = self.check_cond(right);
            return node(Ty::Bool, ExprKind::Binary(op, boxed(left), boxed(right)), pos);
        }
        let literal = |e: &ast::Expr| matches!(&e.kind, ast::ExprKind::Int(_) | ast::ExprKind::Float(_))
            || matches!(&e.kind, ast::ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ast::ExprKind::Int(_) | ast::ExprKind::Float(_)));
        let arithmetic = !op.is_comparison() && !matches!(op, BinOp::In | BinOp::Div);
        let expected = expected.filter(|_| arithmetic);
        let (left, right) = if literal(left) && !literal(right) && op != BinOp::In {
            let right = self.check_expr(right, expected);
            let left = self.check_expr(left, Some(&right.ty));
            (left, right)
        } else {
            let left = self.check_expr(left, expected);
            let hint = if op == BinOp::In { None } else { Some(left.ty.clone()) };
            let right = self.check_expr(right, hint.as_ref());
            (left, right)
        };
        let (left_ty, right_ty) = (self.shallow(&left.ty), self.shallow(&right.ty));
        if matches!(left_ty, Ty::Error) || matches!(right_ty, Ty::Error) {
            return node(Ty::Error, ExprKind::Binary(op, boxed(left), boxed(right)), pos);
        }
        if left_ty == Ty::Dyn || right_ty == Ty::Dyn {
            let left = self.dynamic(left);
            let right = self.dynamic(right);
            let ty = if op == BinOp::In { Ty::Bool } else { Ty::Dyn };
            return node(ty, ExprKind::Binary(op, boxed(left), boxed(right)), pos);
        }
        if matches!(left_ty, Ty::Var(_)) || matches!(right_ty, Ty::Var(_)) && op != BinOp::In {
            self.unify(&left_ty, &right_ty);
        }
        if arithmetic && matches!(self.shallow(&left.ty), Ty::Var(_)) {
            self.hint(&left.ty, expected);
            if matches!(self.shallow(&left.ty), Ty::Var(_)) {
                self.error(pos, "cannot infer the type of these operands, add a type annotation");
                return node(Ty::Error, ExprKind::Binary(op, boxed(left), boxed(right)), pos);
            }
        }
        let (left_ty, right_ty) = (self.shallow(&left.ty), self.shallow(&right.ty));
        let show = |this: &Self| (this.show(&left_ty), this.show(&right_ty));
        let ty = match op {
            BinOp::Eq | BinOp::Ne => {
                if !self.unify(&left_ty, &right_ty) {
                    let (l, r) = show(self);
                    self.error(pos, format!("cannot compare {l} and {r}"));
                }
                Ty::Bool
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = self.builtins["Ord"];
                if !self.unify(&left_ty, &right_ty) {
                    let (l, r) = show(self);
                    self.error(pos, format!("cannot compare {l} and {r}"));
                } else if !self.implements(&left_ty, ord) {
                    let (l, _) = show(self);
                    self.error(pos, format!("{l} does not implement Ord"));
                }
                Ty::Bool
            }
            BinOp::In => {
                let item = match &right_ty {
                    Ty::List(item) | Ty::Set(item) | Ty::Map(item, _) => Some((**item).clone()),
                    Ty::Str => Some(Ty::Str),
                    _ => None,
                };
                if !item.is_some_and(|item| self.unify(&item, &left_ty)) {
                    let (l, r) = show(self);
                    self.error(pos, format!("cannot check {l} in {r}"));
                }
                Ty::Bool
            }
            _ => match (&left_ty, &right_ty, op) {
                (Ty::Int, Ty::Int, BinOp::Div) | (Ty::BigInt, Ty::BigInt, BinOp::Div) => Ty::Float,
                (Ty::Int, Ty::Int, BinOp::MatMul) => self.bad_operands(op, &left_ty, &right_ty, pos),
                (Ty::Int, Ty::Int, _) | (Ty::BigInt, Ty::BigInt, _) => left_ty.clone(),
                (Ty::Float, Ty::Float, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow) => Ty::Float,
                (Ty::Str, Ty::Str, BinOp::Add) => Ty::Str,
                (Ty::List(_), Ty::List(_), BinOp::Add) if self.unify(&left_ty, &right_ty) => left_ty.clone(),
                (Ty::Param(_), Ty::Param(_), _) if left_ty == right_ty => left_ty.clone(),
                _ => self.bad_operands(op, &left_ty, &right_ty, pos),
            },
        };
        node(ty, ExprKind::Binary(op, boxed(left), boxed(right)), pos)
    }

    fn bad_operands(&mut self, op: BinOp, left: &Ty, right: &Ty, pos: Pos) -> Ty {
        let message = format!("cannot apply {} to {} and {}", op.text(), self.show(left), self.show(right));
        self.error(pos, message);
        Ty::Error
    }

    fn check_unary(&mut self, op: UnOp, inner: &ast::Expr, pos: Pos, expected: Option<&Ty>) -> Expr {
        let inner = self.check_expr(inner, if op == UnOp::Not { None } else { expected });
        self.hint(&inner.ty, expected.filter(|_| op != UnOp::Not));
        let ty = match (op, self.shallow(&inner.ty)) {
            (_, Ty::Var(_)) => {
                self.error(pos, "cannot infer the type of this value, add a type annotation");
                Ty::Error
            }
            (_, Ty::Dyn) => Ty::Dyn,
            (_, Ty::Error) => Ty::Error,
            (UnOp::Neg, ty @ (Ty::Int | Ty::Float | Ty::BigInt)) => ty,
            (UnOp::Not, Ty::Bool) => Ty::Bool,
            (UnOp::BitNot, ty @ (Ty::Int | Ty::BigInt)) => ty,
            (_, other) => {
                let symbol = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                    UnOp::BitNot => "~",
                };
                let message = format!("cannot apply {symbol} to {}", self.show(&other));
                self.error(pos, message);
                Ty::Error
            }
        };
        node(ty, ExprKind::Unary(op, boxed(inner)), pos)
    }

    // Names and members:

    fn check_name(&mut self, name: &str, pos: Pos) -> Expr {
        if let Some(local) = self.find_local(name) {
            return node(self.f.locals[local].ty.clone(), ExprKind::Local(local), pos);
        }
        match self.lookup(name) {
            Some(binding) => self.binding_value(binding, name, pos),
            None => {
                self.error(pos, format!("unknown name {name}"));
                node(Ty::Error, ExprKind::Unit, pos)
            }
        }
    }

    fn binding_value(&mut self, binding: Binding, name: &str, pos: Pos) -> Expr {
        match binding {
            Binding::Def(def) => self.def_value(def, pos),
            Binding::Global(global) | Binding::TypedPy(global, _) | Binding::PyModule(global, _) => node(self.globals[global].ty.clone(), ExprKind::Global(global), pos),
            Binding::NiceModule(_) => {
                self.error(pos, format!("module {name} is not a value"));
                node(Ty::Error, ExprKind::Unit, pos)
            }
        }
    }

    fn def_value(&mut self, def: DefId, pos: Pos) -> Expr {
        match self.defs[def].kind.clone() {
            DefKind::Fn(sig) if sig.kind == FnKind::Native => {
                let (params, ret, type_args) = self.instantiate(&sig);
                let ret = if sig.is_async { Ty::Coroutine(Box::new(ret)) } else { ret };
                node(Ty::Fn(params, Box::new(ret)), ExprKind::FnRef(def, type_args), pos)
            }
            DefKind::Variant { parent, fields } if fields.is_empty() => {
                let ty = self.fresh_named(parent);
                node(ty, ExprKind::Construct(def, vec![]), pos)
            }
            DefKind::PyClass { global, .. } => node(Ty::Dyn, ExprKind::Global(global), pos),
            _ => {
                let message = format!("{} cannot be used as a value", self.defs[def].name);
                self.error(pos, message);
                node(Ty::Error, ExprKind::Unit, pos)
            }
        }
    }

    fn fresh_named(&mut self, def: DefId) -> Ty {
        let count = match &self.defs[def].kind {
            DefKind::Struct { generics, .. } | DefKind::Enum { generics, .. } | DefKind::ExternType { generics, .. } => generics.len(),
            _ => 0,
        };
        let args = (0..count).map(|_| self.fresh()).collect();
        Ty::Named(def, args)
    }

    fn type_name(&self, expr: &ast::Expr) -> Option<Binding> {
        let ast::ExprKind::Name(name) = &expr.kind else { return None };
        if self.f.scopes.iter().any(|scope| scope.contains_key(name)) {
            return None;
        }
        if name == "Self" {
            if let Some(Ty::Named(def, _)) = &self.f.self_ty {
                return Some(Binding::Def(*def));
            }
        }
        match self.lookup(name)? {
            Binding::Def(def) if matches!(self.defs[def].kind, DefKind::Struct { .. } | DefKind::Enum { .. } | DefKind::PyClass { .. }) => Some(Binding::Def(def)),
            binding @ (Binding::PyModule(..) | Binding::NiceModule(_)) => Some(binding),
            Binding::TypedPy(global, def) if matches!(self.defs[def].kind, DefKind::ExternType { .. }) => Some(Binding::TypedPy(global, def)),
            _ => None,
        }
    }

    fn static_member(&mut self, owner: Binding, name: &str, pos: Pos) -> Option<Callee> {
        match owner {
            Binding::Def(def) => {
                if let DefKind::Enum { variants, .. } = &self.defs[def].kind {
                    if let Some(&variant) = variants.iter().find(|v| self.defs[**v].name == name) {
                        return Some(Callee::Construct(variant));
                    }
                }
                if let DefKind::PyClass { global, methods, .. } = &self.defs[def].kind {
                    let method = methods.iter().find(|(m, _)| m == name).map(|(_, d)| *d);
                    let attr = node(Ty::Dyn, ExprKind::PyAttr(boxed(node(Ty::Dyn, ExprKind::Global(*global), pos)), name.to_string()), pos);
                    return Some(Callee::Py(attr, method, None));
                }
                let ty = self.fresh_named(def);
                self.find_method(&ty, name).map(|method| Callee::Native(method, None))
            }
            Binding::TypedPy(global, def) => {
                let method = self.find_py_method(def, name);
                let attr = node(Ty::Dyn, ExprKind::PyAttr(boxed(node(Ty::Dyn, ExprKind::Global(global), pos)), name.to_string()), pos);
                Some(Callee::Py(attr, method, None))
            }
            Binding::PyModule(global, module) => {
                let attr = node(Ty::Dyn, ExprKind::PyAttr(boxed(node(Ty::Dyn, ExprKind::Global(global), pos)), name.to_string()), pos);
                let def = self.externs.get(&module).and_then(|names| names.get(name)).copied();
                Some(match def.map(|d| (d, &self.defs[d].kind)) {
                    Some((def, DefKind::ExternType { .. })) => Callee::PyConstruct(attr, def),
                    Some((def, DefKind::Fn(_))) => Callee::Py(attr, Some(def), None),
                    _ => Callee::Py(attr, None, None),
                })
            }
            Binding::NiceModule(module) => match self.scopes[module].get(name).cloned() {
                Some(Binding::Def(def)) if self.defs[def].export => self.def_callee(def, pos),
                _ => None,
            },
            Binding::Global(_) => None,
        }
    }

    fn def_callee(&mut self, def: DefId, pos: Pos) -> Option<Callee> {
        Some(match &self.defs[def].kind {
            DefKind::Fn(sig) if sig.kind == FnKind::PyFunction => return None,
            DefKind::Fn(_) => Callee::Native(def, None),
            DefKind::Struct { .. } | DefKind::Variant { .. } => Callee::Construct(def),
            DefKind::PyClass { global, .. } => Callee::PyConstruct(node(Ty::Dyn, ExprKind::Global(*global), pos), def),
            _ => return None,
        })
    }

    fn check_member(&mut self, object: &ast::Expr, name: &str, pos: Pos) -> Expr {
        if let Some(owner) = self.type_name(object) {
            match owner.clone() {
                Binding::PyModule(global, module) => {
                    let attr = node(Ty::Dyn, ExprKind::PyAttr(boxed(node(Ty::Dyn, ExprKind::Global(global), pos)), name.to_string()), pos);
                    let def = self.externs.get(&module).and_then(|names| names.get(name)).copied();
                    return match def.map(|d| self.defs[d].kind.clone()) {
                        Some(DefKind::Alias(_, ty)) => self.typed(attr, ty),
                        Some(DefKind::Fn(sig)) if sig.generics.is_empty() => {
                            let ty = Ty::Fn(sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(sig.ret.clone()));
                            Expr { ty, ..attr }
                        }
                        _ => attr,
                    };
                }
                Binding::NiceModule(module) => {
                    return match self.scopes[module].get(name).cloned() {
                        Some(binding @ Binding::Def(def)) if self.defs[def].export => self.binding_value(binding, name, pos),
                        _ => {
                            self.error(pos, format!("module has no export {name}"));
                            node(Ty::Error, ExprKind::Unit, pos)
                        }
                    };
                }
                Binding::Def(def) => {
                    if let DefKind::Enum { variants, .. } = &self.defs[def].kind {
                        if let Some(&variant) = variants.iter().find(|v| self.defs[**v].name == name) {
                            return self.def_value(variant, pos);
                        }
                    }
                    let ty = self.fresh_named(def);
                    if let Some(method) = self.find_method(&ty, name) {
                        return self.def_value(method, pos);
                    }
                }
                _ => {}
            }
            if !matches!(owner, Binding::Def(def) if matches!(self.defs[def].kind, DefKind::PyClass { .. })) && !matches!(owner, Binding::TypedPy(..)) {
                self.error(pos, format!("unknown member {name}"));
                return node(Ty::Error, ExprKind::Unit, pos);
            }
        }
        let target = self.check_expr(object, None);
        self.member_of(target, name, pos)
    }

    fn member_of(&mut self, target: Expr, name: &str, pos: Pos) -> Expr {
        match self.shallow(&target.ty) {
            Ty::Named(def, args) => match self.defs[def].kind.clone() {
                DefKind::Struct { fields, .. } => match fields.iter().position(|f| f.name.as_deref() == Some(name)) {
                    Some(index) => node(fields[index].ty.subst(&args), ExprKind::Field(boxed(target), def, index), pos),
                    None => {
                        let message = format!("{} has no field {name}", self.show(&target.ty));
                        self.error(pos, message);
                        node(Ty::Error, ExprKind::Unit, pos)
                    }
                },
                DefKind::PyClass { .. } | DefKind::ExternType { .. } => {
                    let ty = self.py_attr_type(def, &args, name).unwrap_or(Ty::Dyn);
                    let attr = node(Ty::Dyn, ExprKind::PyAttr(boxed(target), name.to_string()), pos);
                    self.typed(attr, ty)
                }
                _ => {
                    let message = format!("{} has no field {name}", self.show(&target.ty));
                    self.error(pos, message);
                    node(Ty::Error, ExprKind::Unit, pos)
                }
            },
            Ty::Dyn => node(Ty::Dyn, ExprKind::PyAttr(boxed(target), name.to_string()), pos),
            Ty::Error => target,
            other => {
                let message = format!("{} has no field {name}", self.show(&other));
                self.error(pos, message);
                node(Ty::Error, ExprKind::Unit, pos)
            }
        }
    }

    pub fn find_py_method(&self, def: DefId, name: &str) -> Option<DefId> {
        match &self.defs[def].kind {
            DefKind::PyClass { methods, bases, .. } => methods.iter().find(|(m, _)| m == name).map(|(_, d)| *d).or_else(|| {
                bases.iter().find_map(|base| self.py_base(def, base).and_then(|base| self.find_py_method(base, name)))
            }),
            DefKind::ExternType { methods, bases, .. } => methods.iter().find(|(m, _)| m == name).map(|(_, d)| *d).or_else(|| {
                bases.iter().find_map(|base| match base {
                    Ty::Named(base, _) => self.find_py_method(*base, name),
                    _ => None,
                })
            }),
            _ => None,
        }
    }

    fn py_base(&self, class: DefId, base: &Expr) -> Option<DefId> {
        let ExprKind::Global(global) = base.kind else { return None };
        self.scopes[self.defs[class].module].values().find_map(|binding| match binding {
            Binding::TypedPy(g, def) if *g == global => Some(*def),
            _ => None,
        })
    }

    fn py_attr_type(&self, def: DefId, args: &[Ty], name: &str) -> Option<Ty> {
        match &self.defs[def].kind {
            DefKind::PyClass { fields, bases, .. } => fields.iter().find(|f| f.name.as_deref() == Some(name)).map(|f| f.ty.clone()).or_else(|| {
                bases.iter().find_map(|base| self.py_base(def, base).and_then(|base| self.py_attr_type(base, &[], name)))
            }),
            DefKind::ExternType { attrs, bases, .. } => attrs.iter().find(|(a, _)| a == name).map(|(_, ty)| ty.subst(args)).or_else(|| {
                bases.iter().find_map(|base| match base {
                    Ty::Named(base, base_args) => self.py_attr_type(*base, base_args, name),
                    _ => None,
                })
            }),
            _ => None,
        }
    }

    pub fn find_method(&self, ty: &Ty, name: &str) -> Option<DefId> {
        let ty = self.shallow(ty);
        let matching: Vec<&Impl> = self.impls.iter().filter(|imp| same_head(&imp.target, &ty)).collect();
        let direct = |inherent: bool| {
            matching
                .iter()
                .filter(|imp| imp.trait_def.is_none() == inherent)
                .find_map(|imp| imp.methods.iter().find(|(m, _)| m == name).map(|(_, d)| *d))
        };
        direct(true).or_else(|| direct(false)).or_else(|| {
            matching.iter().filter_map(|imp| imp.trait_def).find_map(|trait_def| self.trait_method(trait_def, name))
        })
    }

    fn trait_method(&self, trait_def: DefId, name: &str) -> Option<DefId> {
        let DefKind::Trait { methods, .. } = &self.defs[trait_def].kind else { return None };
        methods.iter().find(|(m, _)| m == name).map(|(_, d)| *d)
    }
}
