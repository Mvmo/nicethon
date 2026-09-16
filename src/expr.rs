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
}
