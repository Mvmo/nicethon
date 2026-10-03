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

    // Uses the expected type to fill in type variables, if it fits.
    fn hint(&mut self, ty: &Ty, expected: Option<&Ty>) {
        let Some(expected) = expected else { return };
        let informative = !matches!(self.shallow(expected), Ty::Var(_) | Ty::Dyn | Ty::Unit | Ty::Error | Ty::Never);
        if informative && self.has_vars(ty) && self.compatible(ty, expected) {
            self.unify(ty, expected);
        }
    }

    fn has_vars(&self, ty: &Ty) -> bool {
        let mut found = false;
        self.deep(ty).map(&mut |ty| {
            found |= matches!(ty, Ty::Var(_));
            None
        });
        found
    }

    // Like unify, but only checks and never binds.
    fn compatible(&self, left: &Ty, right: &Ty) -> bool {
        let all = |a: &[Ty], b: &[Ty]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| self.compatible(x, y));
        match (self.shallow(left), self.shallow(right)) {
            (Ty::Var(_), _) | (_, Ty::Var(_)) | (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::List(a), Ty::List(b)) | (Ty::Set(a), Ty::Set(b)) | (Ty::Coroutine(a), Ty::Coroutine(b)) => self.compatible(&a, &b),
            (Ty::Map(a, b), Ty::Map(c, d)) => self.compatible(&a, &c) && self.compatible(&b, &d),
            (Ty::Tuple(a), Ty::Tuple(b)) => all(&a, &b),
            (Ty::Fn(a, r), Ty::Fn(b, s)) => all(&a, &b) && self.compatible(&r, &s),
            (Ty::Named(a, x), Ty::Named(b, y)) => a == b && all(&x, &y),
            (a, b) => a == b,
        }
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

    // Calls:

    fn check_call(&mut self, callee: &ast::Expr, args: &[ast::Arg], pos: Pos, expected: Option<&Ty>) -> Expr {
        let callee = match &callee.kind {
            ast::ExprKind::Name(name) if self.f.scopes.iter().all(|s| !s.contains_key(name)) => match self.lookup(name) {
                Some(Binding::Def(def)) => match self.def_callee(def, pos) {
                    Some(callee) => callee,
                    None => {
                        let value = self.def_value(def, pos);
                        self.value_callee(value)
                    }
                },
                Some(Binding::TypedPy(global, def)) => {
                    let value = node(Ty::Dyn, ExprKind::Global(global), pos);
                    match self.defs[def].kind {
                        DefKind::ExternType { .. } => Callee::PyConstruct(value, def),
                        DefKind::Fn(_) => Callee::Py(value, Some(def), None),
                        _ => Callee::Py(value, None, None),
                    }
                }
                Some(binding) => {
                    let value = self.binding_value(binding, name, pos);
                    self.value_callee(value)
                }
                None => {
                    self.error(pos, format!("unknown name {name}"));
                    return node(Ty::Error, ExprKind::Unit, pos);
                }
            },
            ast::ExprKind::Member(object, name) => {
                let static_callee = match self.type_name(object) {
                    Some(owner) => self.static_member(owner, name, pos),
                    None => None,
                };
                match static_callee {
                    Some(callee) => callee,
                    None if matches!(object.kind, ast::ExprKind::Super) => {
                        let target = self.super_value(pos);
                        Callee::Py(node(Ty::Dyn, ExprKind::PyAttr(boxed(target), name.clone()), pos), None, None)
                    }
                    None => {
                        let target = self.check_expr(object, None);
                        match self.method_callee(target, name, pos) {
                            Some(callee) => callee,
                            None => return node(Ty::Error, ExprKind::Unit, pos),
                        }
                    }
                }
            }
            _ => {
                let value = self.check_expr(callee, None);
                self.value_callee(value)
            }
        };
        self.apply(callee, args, pos, expected)
    }

    fn value_callee(&mut self, value: Expr) -> Callee {
        match self.shallow(&value.ty) {
            Ty::Fn(..) => Callee::Value(value),
            ty @ Ty::Named(def, _) if matches!(self.defs[def].kind, DefKind::ExternType { .. }) => {
                let method = self.find_py_method(def, "__call__");
                let value = self.dynamic(value);
                Callee::Py(value, method, Some(ty))
            }
            _ => {
                let value = self.dynamic(value);
                Callee::Py(value, None, None)
            }
        }
    }

    fn method_callee(&mut self, target: Expr, name: &str, pos: Pos) -> Option<Callee> {
        let ty = self.shallow(&target.ty);
        let attr = |this: &mut Self, target: Expr| node(Ty::Dyn, ExprKind::PyAttr(boxed(this.dynamic(target)), name.to_string()), pos);
        match &ty {
            Ty::Dyn | Ty::Error => return Some(Callee::Py(attr(self, target), None, None)),
            Ty::Named(def, _) if name == "copy" && matches!(self.defs[*def].kind, DefKind::Struct { .. }) => return Some(Callee::Copy(target, *def)),
            Ty::Named(def, _) if matches!(self.defs[*def].kind, DefKind::PyClass { .. } | DefKind::ExternType { .. }) => {
                let method = self.find_py_method(*def, name);
                return Some(Callee::Py(attr(self, target), method, Some(ty.clone())));
            }
            Ty::Param(index) => {
                let bounds = self.f.bounds.get(*index as usize).cloned().unwrap_or_default();
                if let Some((method, args)) = bounds.iter().find_map(|(b, args)| Some((self.trait_method(*b, name)?, args.clone()))) {
                    return Some(Callee::Trait(target, method, args));
                }
            }
            _ => {}
        }
        if let Some(method) = self.find_method(&ty, name) {
            return Some(Callee::Native(method, Some(target)));
        }
        if let Ty::Named(def, args) = &ty {
            if let Some(index) = self.fields_of(*def).iter().position(|f| f.name.as_deref() == Some(name)) {
                let field_ty = self.fields_of(*def)[index].ty.subst(args);
                if matches!(self.shallow(&field_ty), Ty::Fn(..)) {
                    return Some(Callee::Value(node(field_ty, ExprKind::Field(boxed(target), *def, index), pos)));
                }
            }
        }
        let message = format!("{} has no method {name}", self.show(&ty));
        self.error(pos, message);
        None
    }

    fn apply(&mut self, callee: Callee, args: &[ast::Arg], pos: Pos, expected: Option<&Ty>) -> Expr {
        match callee {
            Callee::Native(def, receiver) => {
                let sig = self.sig(def).clone();
                let (params, ret, type_args) = self.instantiate(&sig);
                let mut values = vec![];
                let offset = receiver.is_some() as usize;
                if let Some(receiver) = receiver {
                    self.unify(&params[0], &receiver.ty);
                    // A trait method used on a concrete type: the impl tells the trait's type arguments.
                    if let Some(owner) = sig.owner.filter(|o| matches!(self.defs[*o].kind, DefKind::Trait { .. })) {
                        if let Some(trait_args) = self.trait_args_for(&receiver.ty, owner) {
                            for (arg, trait_arg) in type_args[1..].iter().zip(&trait_args) {
                                self.unify(arg, trait_arg);
                            }
                        }
                    }
                    values.push(receiver);
                }
                let ret = if sig.is_async { Ty::Coroutine(Box::new(ret)) } else { ret };
                self.hint(&ret, expected);
                let matched = self.match_args(&sig.params[offset..], &params[offset..], args, pos);
                values.extend(self.ordered(matched, &sig.params[offset..], pos));
                self.check_bounds(&sig, &type_args, pos);
                if sig.kind == FnKind::TraitMethod {
                    return node(ret, ExprKind::TraitCall(def, type_args, values), pos);
                }
                node(ret, ExprKind::Call(def, type_args, values), pos)
            }
            Callee::Trait(receiver, method, trait_args) => {
                let sig = self.sig(method).clone();
                let (params, ret, type_args) = self.instantiate(&sig);
                self.unify(&params[0], &receiver.ty);
                for (arg, trait_arg) in type_args[1..].iter().zip(&trait_args) {
                    self.unify(arg, trait_arg);
                }
                self.hint(&ret, expected);
                let mut values = vec![receiver];
                let matched = self.match_args(&sig.params[1..], &params[1..], args, pos);
                values.extend(self.ordered(matched, &sig.params[1..], pos));
                node(ret, ExprKind::TraitCall(method, type_args, values), pos)
            }
            Callee::Construct(def) => {
                let (owner, fields) = match &self.defs[def].kind {
                    DefKind::Variant { parent, fields } => (*parent, fields.clone()),
                    _ => (def, self.fields_of(def).to_vec()),
                };
                let ty = self.fresh_named(owner);
                self.hint(&ty, expected);
                let Ty::Named(_, type_args) = &ty else { unreachable!() };
                let params: Vec<Param> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| Param { name: f.name.clone().unwrap_or_else(|| i.to_string()), ty: f.ty.clone(), default: f.default.clone(), optional: false, kind: ast::ParamKind::Normal })
                    .collect();
                let tys: Vec<Ty> = params.iter().map(|p| p.ty.subst(type_args)).collect();
                let matched = self.match_args(&params, &tys, args, pos);
                let values = self.ordered(matched, &params, pos);
                node(ty, ExprKind::Construct(def, values), pos)
            }
            Callee::Copy(target, def) => {
                let Ty::Named(_, type_args) = self.shallow(&target.ty) else { unreachable!() };
                let fields = self.fields_of(def).to_vec();
                let mut updates = vec![];
                for arg in args {
                    let ast::Arg::Named(name, value) = arg else {
                        self.error(pos, "copy only takes named arguments");
                        continue;
                    };
                    let Some(index) = fields.iter().position(|f| f.name.as_deref() == Some(name)) else {
                        self.error(value.pos, format!("{} has no field {name}", self.defs[def].name));
                        continue;
                    };
                    let ty = fields[index].ty.subst(&type_args);
                    let value = self.check_expr(value, Some(&ty));
                    updates.push((index, self.coerce(value, &ty)));
                }
                let ty = target.ty.clone();
                node(ty, ExprKind::CopyWith(boxed(target), def, updates), pos)
            }
            Callee::Value(value) => {
                let Ty::Fn(params, ret) = self.shallow(&value.ty) else { unreachable!() };
                let infos: Vec<Param> = params.iter().enumerate().map(|(i, ty)| Param { name: i.to_string(), ty: ty.clone(), default: None, optional: false, kind: ast::ParamKind::Normal }).collect();
                if args.iter().any(|a| !matches!(a, ast::Arg::Pos(_))) {
                    self.error(pos, "function values only take positional arguments");
                }
                let matched = self.match_args(&infos, &params, args, pos);
                let values = self.ordered(matched, &infos, pos);
                node(*ret, ExprKind::CallValue(boxed(value), values), pos)
            }
            Callee::Py(function, sig_def, receiver) => {
                let typed = sig_def.map(|def| {
                    let sig = self.sig(def).clone();
                    let skip = (sig.kind == FnKind::PyMethod || sig.owner.is_some_and(|o| matches!(self.defs[o].kind, DefKind::PyClass { .. }))) as usize;
                    (sig, skip)
                });
                let function = self.dynamic(function);
                match typed {
                    Some((sig, skip)) => {
                        let (params, ret, _) = self.instantiate(&sig);
                        let skip = skip.min(params.len());
                        if let (Some(receiver), 1) = (&receiver, skip) {
                            self.unify(&params[0], receiver);
                        }
                        self.hint(&ret, expected);
                        let py_args = self.py_args(&sig.params[skip..], &params[skip..], args, pos);
                        let call = node(Ty::Dyn, ExprKind::PyCall(boxed(function), py_args), pos);
                        let ret = if sig.is_async { Ty::Coroutine(Box::new(ret)) } else { ret };
                        self.typed(call, ret)
                    }
                    None => {
                        let py_args = self.untyped_args(args);
                        node(Ty::Dyn, ExprKind::PyCall(boxed(function), py_args), pos)
                    }
                }
            }
            Callee::PyConstruct(class, def) => {
                let ty = self.fresh_named(def);
                let Ty::Named(_, type_args) = &ty else { unreachable!() };
                let init = self.find_py_method(def, "__init__");
                let class = self.dynamic(class);
                let py_args = match init {
                    Some(init) => {
                        let sig = self.sig(init).clone();
                        let mut all_args = type_args.clone();
                        all_args.extend(sig.generics[type_args.len().min(sig.generics.len())..].iter().map(|_| self.fresh()));
                        let params: Vec<Ty> = sig.params.iter().map(|p| p.ty.subst(&all_args)).collect();
                        let skip = (!params.is_empty()) as usize;
                        self.py_args(&sig.params[skip..], &params[skip..], args, pos)
                    }
                    None => self.untyped_args(args),
                };
                let call = node(Ty::Dyn, ExprKind::PyCall(boxed(class), py_args), pos);
                Expr { ty, ..call }
            }
        }
    }

    fn check_bounds(&mut self, sig: &FnSig, type_args: &[Ty], pos: Pos) {
        for ((_, bounds), arg) in sig.generics.iter().zip(type_args) {
            for (bound, bound_args) in bounds {
                match self.trait_args_for(arg, *bound) {
                    Some(actual) => {
                        for (have, want) in actual.iter().zip(bound_args) {
                            self.unify(have, &want.subst(type_args));
                        }
                    }
                    None => {
                        let message = format!("{} does not implement {}", self.show(arg), self.defs[*bound].name);
                        self.error(pos, message);
                    }
                }
            }
        }
    }

    fn untyped_args(&mut self, args: &[ast::Arg]) -> Vec<PyArg> {
        args.iter()
            .map(|arg| {
                let (value, wrap): (&ast::Expr, fn(Expr) -> PyArg) = match arg {
                    ast::Arg::Pos(value) => (value, PyArg::Pos),
                    ast::Arg::Star(value) => (value, PyArg::Star),
                    ast::Arg::StarStar(value) => (value, PyArg::StarStar),
                    ast::Arg::Named(name, value) => {
                        let value = self.check_expr(value, Some(&Ty::Dyn));
                        let value = self.dynamic(value);
                        return PyArg::Named(name.clone(), value);
                    }
                };
                let value = self.check_expr(value, Some(&Ty::Dyn));
                wrap(self.dynamic(value))
            })
            .collect()
    }

    fn py_args(&mut self, infos: &[Param], tys: &[Ty], args: &[ast::Arg], pos: Pos) -> Vec<PyArg> {
        let star = infos.iter().position(|p| p.kind == ast::ParamKind::Star);
        let has_kwargs = infos.iter().any(|p| p.kind == ast::ParamKind::StarStar);
        let regular: Vec<usize> = (0..infos.len()).filter(|i| infos[*i].kind == ast::ParamKind::Normal).collect();
        let mut used = vec![false; infos.len()];
        let mut next = 0;
        let mut result = vec![];
        let mut lambdas = vec![];
        for (index, arg) in args.iter().enumerate() {
            let (slot, value) = match arg {
                ast::Arg::Pos(value) => {
                    let slot = if next < regular.len() {
                        next += 1;
                        Some(regular[next - 1])
                    } else {
                        star
                    };
                    if slot.is_none() {
                        self.error(value.pos, "too many arguments");
                    }
                    (slot, value)
                }
                ast::Arg::Named(name, value) => {
                    let slot = regular.iter().copied().find(|i| infos[*i].name == *name);
                    if slot.is_none() && !has_kwargs {
                        self.error(value.pos, format!("unknown argument {name}"));
                    }
                    (slot, value)
                }
                ast::Arg::Star(value) | ast::Arg::StarStar(value) => (None, value),
            };
            if let Some(slot) = slot {
                used[slot] = true;
            }
            if matches!(value.kind, ast::ExprKind::Lambda(..)) {
                lambdas.push((index, slot, value));
                result.push(None);
                continue;
            }
            result.push(Some(self.py_arg(arg, slot.map(|s| &tys[s]), value)));
        }
        for (index, slot, value) in lambdas {
            result[index] = Some(self.py_arg(&args[index], slot.map(|s| &tys[s]), value));
        }
        for (i, info) in infos.iter().enumerate() {
            if info.kind == ast::ParamKind::Normal && !used[i] && !info.optional && info.default.is_none() {
                self.error(pos, format!("missing argument {}", info.name));
            }
        }
        result.into_iter().flatten().collect()
    }

    fn py_arg(&mut self, arg: &ast::Arg, ty: Option<&Ty>, value: &ast::Expr) -> PyArg {
        let ty = ty.cloned().unwrap_or(Ty::Dyn);
        let checked = self.check_expr(value, Some(&ty));
        let checked = self.coerce(checked, &ty);
        let checked = self.dynamic(checked);
        match arg {
            ast::Arg::Named(name, _) => PyArg::Named(name.clone(), checked),
            ast::Arg::Star(_) => PyArg::Star(checked),
            ast::Arg::StarStar(_) => PyArg::StarStar(checked),
            ast::Arg::Pos(_) => PyArg::Pos(checked),
        }
    }

    fn match_args(&mut self, infos: &[Param], tys: &[Ty], args: &[ast::Arg], pos: Pos) -> Vec<Option<Expr>> {
        let mut slots: Vec<Option<&ast::Expr>> = vec![None; infos.len()];
        let mut next = 0;
        for arg in args {
            match arg {
                ast::Arg::Pos(value) => {
                    if next >= infos.len() {
                        self.error(value.pos, "too many arguments");
                    } else {
                        slots[next] = Some(value);
                    }
                    next += 1;
                }
                ast::Arg::Named(name, value) => match infos.iter().position(|p| p.name == *name) {
                    Some(index) if slots[index].is_some() => self.error(value.pos, format!("argument {name} given twice")),
                    Some(index) => slots[index] = Some(value),
                    None => self.error(value.pos, format!("unknown argument {name}")),
                },
                ast::Arg::Star(value) | ast::Arg::StarStar(value) => self.error(value.pos, "spread arguments are only allowed in python calls"),
            }
        }
        let mut values: Vec<Option<Expr>> = vec![None; infos.len()];
        for lambdas in [false, true] {
            for (index, slot) in slots.iter().enumerate() {
                let Some(value) = slot else { continue };
                if matches!(value.kind, ast::ExprKind::Lambda(..)) != lambdas {
                    continue;
                }
                let checked = self.check_expr(value, Some(&tys[index]));
                values[index] = Some(self.coerce(checked, &tys[index]));
            }
        }
        for (index, info) in infos.iter().enumerate() {
            if values[index].is_none() && slots[index].is_none() && info.default.is_none() && !info.optional {
                self.error(pos, format!("missing argument {}", info.name));
            }
        }
        values
    }

    fn ordered(&mut self, matched: Vec<Option<Expr>>, infos: &[Param], pos: Pos) -> Vec<Expr> {
        matched
            .into_iter()
            .zip(infos)
            .map(|(value, info)| value.or_else(|| info.default.clone()).unwrap_or_else(|| node(Ty::Error, ExprKind::Unit, pos)))
            .collect()
    }

    fn index_parts(&mut self, args: &[ast::IndexArg]) -> Vec<PyIndexPart> {
        let dynamic = |this: &mut Self, expr: &Option<ast::Expr>| {
            expr.as_ref().map(|e| {
                let value = this.check_expr(e, None);
                boxed(this.dynamic(value))
            })
        };
        args.iter()
            .map(|arg| match arg {
                ast::IndexArg::Expr(e) => {
                    let value = self.check_expr(e, None);
                    PyIndexPart::Expr(self.dynamic(value))
                }
                ast::IndexArg::Slice(start, stop, step) => PyIndexPart::Slice(dynamic(self, start), dynamic(self, stop), dynamic(self, step)),
                ast::IndexArg::Ellipsis => PyIndexPart::Ellipsis,
            })
            .collect()
    }

    // Match and patterns:

    fn check_match(&mut self, scrutinee: &ast::Expr, arms: &[ast::Arm], expected: Option<&Ty>, pos: Pos) -> Expr {
        let scrutinee = self.check_expr(scrutinee, None);
        let ty = scrutinee.ty.clone();
        let mut result = None;
        let mut checked = vec![];
        for arm in arms {
            let arm = self.scoped(|this| {
                let pattern = this.check_pattern(&arm.pattern, &ty);
                let guard = arm.guard.as_ref().map(|g| this.check_cond(g));
                let body = this.check_expr(&arm.body, expected);
                let body = this.branch(body, expected, &mut result);
                Arm { pattern, guard, body }
            });
            checked.push(arm);
        }
        self.check_exhaustive(&ty, &checked, pos);
        let ty = result.unwrap_or(Ty::Never);
        node(ty, ExprKind::Match(boxed(scrutinee), checked), pos)
    }

    pub fn irrefutable(&self, pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatKind::Wild | PatKind::Bind(_) => true,
            PatKind::Tuple(items) => items.iter().all(|p| self.irrefutable(p)),
            PatKind::Or(alternatives) => alternatives.iter().any(|p| self.irrefutable(p)),
            PatKind::Variant(def, fields) => {
                let single = match &self.defs[*def].kind {
                    DefKind::Variant { parent, .. } => matches!(&self.defs[*parent].kind, DefKind::Enum { variants, .. } if variants.len() == 1),
                    _ => true,
                };
                single && fields.iter().all(|(_, p)| self.irrefutable(p))
            }
            _ => false,
        }
    }

    fn check_exhaustive(&mut self, ty: &Ty, arms: &[Arm], pos: Pos) {
        let rows = arms.iter().filter(|arm| arm.guard.is_none()).map(|arm| vec![arm.pattern.clone()]).collect();
        if let Some(witness) = self.uncovered(rows, &[self.deep(ty)]) {
            self.error(pos, format!("non-exhaustive match: {} not covered", witness[0]));
        }
    }

    // Pattern matrix check: returns example values (one per column) that no
    // row matches, or None when the rows cover everything.
    fn uncovered(&self, rows: Vec<Vec<Pattern>>, tys: &[Ty]) -> Option<Vec<String>> {
        let Some((first, rest)) = tys.split_first() else {
            return rows.is_empty().then(Vec::new);
        };
        let mut expanded = vec![];
        for row in rows {
            match &row[0].kind {
                PatKind::Or(alternatives) => expanded.extend(alternatives.iter().map(|alternative| [vec![alternative.clone()], row[1..].to_vec()].concat())),
                _ => expanded.push(row),
            }
        }
        let wild = |pattern: &Pattern| matches!(pattern.kind, PatKind::Wild | PatKind::Bind(_));
        let constructors = self.constructors(first);
        if let Some(constructors) = constructors.filter(|_| expanded.iter().any(|row| !wild(&row[0]))) {
            for (constructor, name, fields) in constructors {
                let rows = expanded
                    .iter()
                    .filter_map(|row| Some([self.specialize(&row[0], &constructor, &fields)?, row[1..].to_vec()].concat()))
                    .collect();
                if let Some(witness) = self.uncovered(rows, &[fields.clone(), rest.to_vec()].concat()) {
                    let (inner, outer) = witness.split_at(fields.len());
                    let shown = match constructor {
                        Constructor::Tuple => format!("({})", inner.join(", ")),
                        _ if inner.is_empty() => name,
                        _ => format!("{name}({})", inner.join(", ")),
                    };
                    return Some([vec![shown], outer.to_vec()].concat());
                }
            }
            return None;
        }
        let rows = expanded.into_iter().filter(|row| wild(&row[0])).map(|row| row[1..].to_vec()).collect();
        let mut witness = self.uncovered(rows, rest)?;
        let example = match self.constructors(first).and_then(|c| c.into_iter().next()) {
            Some((_, name, fields)) if fields.is_empty() => name,
            Some((Constructor::Tuple, _, fields)) => format!("({})", vec!["_"; fields.len()].join(", ")),
            Some((_, name, fields)) => format!("{name}({})", vec!["_"; fields.len()].join(", ")),
            None => "_".into(),
        };
        witness.insert(0, example);
        Some(witness)
    }

    fn constructors(&self, ty: &Ty) -> Option<Vec<(Constructor, String, Vec<Ty>)>> {
        let fields = |def: DefId, args: &[Ty]| self.fields_of(def).iter().map(|f| f.ty.subst(args)).collect::<Vec<_>>();
        match self.shallow(ty) {
            Ty::Bool => Some(vec![(Constructor::Bool(true), "true".into(), vec![]), (Constructor::Bool(false), "false".into(), vec![])]),
            Ty::Tuple(items) => Some(vec![(Constructor::Tuple, String::new(), items)]),
            Ty::Named(def, args) => match &self.defs[def].kind {
                DefKind::Enum { variants, .. } => Some(variants.iter().map(|v| (Constructor::Variant(*v), self.defs[*v].name.clone(), fields(*v, &args))).collect()),
                DefKind::Struct { .. } => Some(vec![(Constructor::Variant(def), self.defs[def].name.clone(), fields(def, &args))]),
                _ => None,
            },
            _ => None,
        }
    }

    fn specialize(&self, pattern: &Pattern, constructor: &Constructor, fields: &[Ty]) -> Option<Vec<Pattern>> {
        let wilds = || fields.iter().map(|ty| Pattern { ty: ty.clone(), kind: PatKind::Wild }).collect::<Vec<_>>();
        match (&pattern.kind, constructor) {
            (PatKind::Wild | PatKind::Bind(_), _) => Some(wilds()),
            (PatKind::Variant(def, subs), Constructor::Variant(expected)) if def == expected => {
                let mut result = wilds();
                for (index, sub) in subs {
                    result[*index] = sub.clone();
                }
                Some(result)
            }
            (PatKind::Tuple(items), Constructor::Tuple) => Some(items.clone()),
            (PatKind::Lit(literal), Constructor::Bool(value)) => matches!(literal.kind, ExprKind::Bool(b) if b == *value).then(Vec::new),
            _ => None,
        }
    }

    pub fn check_pattern(&mut self, pattern: &ast::Pattern, ty: &Ty) -> Pattern {
        let pos = pattern.pos;
        let kind = match &pattern.kind {
            ast::PatKind::Wild => PatKind::Wild,
            ast::PatKind::Lit(literal) => {
                let value = self.check_expr(literal, Some(ty));
                let value = self.coerce(value, ty);
                PatKind::Lit(boxed(value))
            }
            ast::PatKind::Ident(name) => {
                let variant = match self.shallow(ty) {
                    Ty::Named(def, _) => match &self.defs[def].kind {
                        DefKind::Enum { variants, .. } => variants.iter().copied().find(|v| self.defs[*v].name == *name),
                        _ => None,
                    },
                    _ => None,
                };
                match variant {
                    Some(variant) => PatKind::Variant(variant, vec![]),
                    None if name == "None" && matches!(self.shallow(ty), Ty::Dyn) => PatKind::PyNone,
                    None => PatKind::Bind(self.declare_local(name, ty.clone(), false)),
                }
            }
            ast::PatKind::TypeTest(name, target) => {
                if !matches!(self.shallow(ty), Ty::Dyn | Ty::Error) {
                    let message = format!("type tests need a Dyn value, found {}", self.show(ty));
                    self.error(pos, message);
                }
                let target = self.resolve_here(target);
                PatKind::TypeTest(self.declare_local(name, target, false))
            }
            ast::PatKind::Ctor(path, subs) => self.ctor_pattern(path, subs, ty, pos),
            ast::PatKind::Tuple(subs) => {
                let items = match self.shallow(ty) {
                    Ty::Tuple(items) if items.len() == subs.len() => items,
                    Ty::Var(_) => {
                        let items: Vec<Ty> = subs.iter().map(|_| self.fresh()).collect();
                        self.unify(ty, &Ty::Tuple(items.clone()));
                        items
                    }
                    other => {
                        let message = format!("tuple pattern does not match {}", self.show(&other));
                        self.error(pos, message);
                        vec![Ty::Error; subs.len()]
                    }
                };
                let patterns = subs.iter().zip(&items).map(|(sub, item)| self.sub_pattern(sub, item)).collect();
                PatKind::Tuple(patterns)
            }
            ast::PatKind::List(subs) => {
                let item = match self.shallow(ty) {
                    Ty::List(item) => *item,
                    other => {
                        let message = format!("list pattern does not match {}", self.show(&other));
                        self.error(pos, message);
                        Ty::Error
                    }
                };
                let rest = matches!(subs.last(), Some(ast::SubPat::Rest));
                let items = subs.iter().filter(|s| !matches!(s, ast::SubPat::Rest)).map(|sub| self.sub_pattern(sub, &item)).collect();
                PatKind::List(items, rest)
            }
            ast::PatKind::Or(alternatives) => {
                let before = self.f.locals.len();
                let patterns = alternatives.iter().map(|alt| self.check_pattern(alt, ty)).collect();
                if self.f.locals.len() != before {
                    self.error(pos, "alternatives in a pattern cannot bind names");
                }
                PatKind::Or(patterns)
            }
        };
        Pattern { ty: ty.clone(), kind }
    }

    fn sub_pattern(&mut self, sub: &ast::SubPat, ty: &Ty) -> Pattern {
        match sub {
            ast::SubPat::Pat(pattern) | ast::SubPat::Named(_, pattern) => self.check_pattern(pattern, ty),
            ast::SubPat::Rest => Pattern { ty: ty.clone(), kind: PatKind::Wild },
        }
    }

    fn ctor_pattern(&mut self, path: &[String], subs: &[ast::SubPat], ty: &Ty, pos: Pos) -> PatKind {
        let scrutinee = self.shallow(ty);
        let from_type = match (&scrutinee, path) {
            (Ty::Named(def, _), [name]) => match &self.defs[*def].kind {
                DefKind::Enum { variants, .. } => variants.iter().copied().find(|v| self.defs[*v].name == *name),
                DefKind::Struct { .. } if self.defs[*def].name == *name => Some(*def),
                _ => None,
            },
            _ => None,
        };
        let def = from_type.or_else(|| match (self.lookup(&path[0]), path.len()) {
            (Some(Binding::Def(def)), 1) => Some(def),
            (Some(Binding::Def(def)), 2) => match &self.defs[def].kind {
                DefKind::Enum { variants, .. } => variants.iter().copied().find(|v| self.defs[*v].name == path[1]),
                _ => None,
            },
            _ => None,
        });
        let Some(def) = def.filter(|d| matches!(self.defs[*d].kind, DefKind::Variant { .. } | DefKind::Struct { .. })) else {
            self.error(pos, format!("unknown variant {}", path.join(".")));
            return PatKind::Wild;
        };
        let (owner, fields) = match &self.defs[def].kind {
            DefKind::Variant { parent, fields } => (*parent, fields.clone()),
            _ => (def, self.fields_of(def).to_vec()),
        };
        let named = self.fresh_named(owner);
        if !self.unify(&named, ty) {
            let message = format!("pattern {} does not match {}", path.join("."), self.show(ty));
            self.error(pos, message);
        }
        let Ty::Named(_, type_args) = named else { unreachable!() };
        let has_rest = subs.iter().any(|s| matches!(s, ast::SubPat::Rest));
        let positional = subs.iter().filter(|s| matches!(s, ast::SubPat::Pat(_))).count();
        if !has_rest && subs.len() != fields.len() {
            self.error(pos, format!("{} has {} fields, the pattern has {}", path.join("."), fields.len(), subs.len()));
        }
        let mut result = vec![];
        for (index, sub) in subs.iter().enumerate() {
            let field = match sub {
                ast::SubPat::Rest => continue,
                ast::SubPat::Pat(_) if index < positional && index < fields.len() => index,
                ast::SubPat::Named(name, _) => match fields.iter().position(|f| f.name.as_deref() == Some(name)) {
                    Some(field) => field,
                    None => {
                        self.error(pos, format!("{} has no field {name}", path.join(".")));
                        continue;
                    }
                },
                ast::SubPat::Pat(_) => continue,
            };
            let field_ty = fields[field].ty.subst(&type_args);
            result.push((field, self.sub_pattern(sub, &field_ty)));
        }
        PatKind::Variant(def, result)
    }
}

fn path_text(expr: &ast::Expr) -> String {
    match &expr.kind {
        ast::ExprKind::Name(name) => name.clone(),
        ast::ExprKind::Member(object, field) => format!("{}.{field}", path_text(object)),
        ast::ExprKind::SelfValue => "self".into(),
        _ => "expression".into(),
    }
}

fn pattern_pos(stmt: &ast::Stmt) -> Pos {
    match stmt {
        ast::Stmt::Let(pattern, ..) => pattern.pos,
        _ => 0,
    }
}
