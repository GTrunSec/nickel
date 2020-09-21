//! Implementation of the typechecker.
//!
//! # Mode
//!
//! Typechecking can be made in to different modes:
//! - **Strict**: correspond to traditional typechecking in strongly, statically typed languages.
//! This happens inside a `Promise` block.
//! - **Non strict**: do not enforce any typing, but still store the annotations of let bindings in
//! the environment, and continue to traverse the AST looking for other `Promise` blocks to
//! typecheck.
//!
//! The algorithm starts in non strict mode. It is switched to strict mode when entering a
//! `Promise` block, and is switched to non-strict mode when entering an `Assume` block.  `Promise`
//! and `Assume` thus serve both two purposes: annotate a term with a type, and set the
//! typechecking mode.
//!
//! # Type inference
//!
//! Type inference is done via a standard unification algorithm. Inference is limited, since the type
//! of a let binding is currently never inferred (let alone generalized): it must be annotated via
//! a `Promise` or an `Assume`, or it is given the type `Dyn`, no matter what is the typechecking
//! mode.
use crate::error::TypecheckError;
use crate::identifier::Ident;
use crate::program::ImportResolver;
use crate::term::{BinaryOp, RichTerm, StrChunk, Term, UnaryOp};
use crate::types::{AbsType, Types};
use std::collections::{HashMap, HashSet};

#[derive(Debug, PartialEq)]
enum RowUnifError {
    MissingRow(),
    IllformedRow(TypeWrapper),
    IncompatibleConstraints(),
    ConstraintFailed(Ident),
}

type Environment = HashMap<Ident, TypeWrapper>;

/// The state of unification.
pub struct State<'a> {
    resolver: &'a mut dyn ImportResolver,
    table: &'a mut GTypes,
    constr: &'a mut GConstr,
}

impl<'a> State<'a> {
    pub fn new(
        resolver: &'a mut dyn ImportResolver,
        table: &'a mut GTypes,
        constr: &'a mut GConstr,
    ) -> Self {
        State {
            resolver,
            table,
            constr,
        }
    }
}

/// Typecheck a term.
///
/// Return the inferred type in case of success. This is just a wrapper that calls
/// [`type_check_`](fn.type_check_.html) with a fresh unification variable as goal.
pub fn type_check(
    t: &RichTerm,
    resolver: &mut dyn ImportResolver,
) -> Result<Types, TypecheckError> {
    let mut table = GTypes::new();
    let mut constr = GConstr::new();
    let ty = TypeWrapper::Ptr(new_var(&mut table));
    type_check_(
        &mut State::new(resolver, &mut table, &mut constr),
        Environment::new(),
        false,
        t,
        ty.clone(),
    )?;

    Ok(to_type(&table, ty))
}

/// Typecheck a term against a specific type.
///
/// # Arguments
///
/// - `state.env`: maps variable of the environment to a type.
/// - `state` : the unification table (see [`GTypes`](type.GTypes.html)).
/// - `constr`: row constraints (see [`GConstr`](type.GConstr.html)).
/// - `resolver`: an import resolver, to retrieve and typecheck imports.
/// - `t`: the term to check.
/// - `ty`: the type to check the term against.
/// - `strict`: the typechecking mode.
fn type_check_(
    state: &mut State,
    mut env: Environment,
    strict: bool,
    rt: &RichTerm,
    ty: TypeWrapper,
) -> Result<(), TypecheckError> {
    let RichTerm { term: t, pos } = rt;
    match t.as_ref() {
        Term::Bool(_) => unify(
            state,
            env,
            strict,
            ty,
            TypeWrapper::Concrete(AbsType::Bool()),
        ),
        Term::Num(_) => unify(
            state,
            env,
            strict,
            ty,
            TypeWrapper::Concrete(AbsType::Num()),
        ),
        Term::Str(_) => unify(
            state,
            env,
            strict,
            ty,
            TypeWrapper::Concrete(AbsType::Str()),
        ),
        Term::StrChunks(chunks) => {
            unify(
                state,
                env.clone(),
                strict,
                ty,
                TypeWrapper::Concrete(AbsType::Str()),
            )?;

            chunks
                .iter()
                .try_for_each(|chunk| -> Result<(), TypecheckError> {
                    match chunk {
                        StrChunk::Literal(_) => Ok(()),
                        StrChunk::Expr(t) => type_check_(
                            state,
                            env.clone(),
                            strict,
                            t,
                            TypeWrapper::Concrete(AbsType::Dyn()),
                        ),
                    }
                })
        }
        Term::Fun(x, rt) => {
            let src = TypeWrapper::Ptr(new_var(&mut state.table));
            // TODO what to do here, this makes more sense to me, but it means let x = foo in bar
            // behaves quite different to (\x.bar) foo, worth considering if it's ok to type these two differently
            // let src = TypeWrapper::The(AbsType::Dyn());
            let trg = TypeWrapper::Ptr(new_var(&mut state.table));
            let arr =
                TypeWrapper::Concrete(AbsType::arrow(Box::new(src.clone()), Box::new(trg.clone())));

            unify(state, env.clone(), strict, ty, arr)?;

            env.insert(x.clone(), src);
            type_check_(state, env, strict, rt, trg)
        }
        Term::List(terms) => {
            unify(
                state,
                env.clone(),
                strict,
                ty,
                TypeWrapper::Concrete(AbsType::List()),
            )?;

            terms
                .iter()
                .try_for_each(|t| -> Result<(), TypecheckError> {
                    // Since lists elements are checked against the type `Dyn`, it does not make sense
                    // to typecheck them even in strict mode, as this will always fails, unless they
                    // are annotated with an `Assume(Dyn, ..)`, which will always succeed.
                    type_check_(
                        state,
                        env.clone(),
                        false,
                        t,
                        TypeWrapper::Concrete(AbsType::Dyn()),
                    )
                })
        }
        Term::Lbl(_) => {
            // TODO implement lbl type
            unify(
                state,
                env,
                strict,
                ty,
                TypeWrapper::Concrete(AbsType::Dyn()),
            )
        }
        Term::Let(x, e, t) => {
            // If the right hand side has a Promise or Assume, we use it as a
            // type annotation otherwise, x gets type Dyn
            let exp = match e.as_ref() {
                Term::Assume(ty, _, _) | Term::Promise(ty, _, _) => to_typewrapper(ty.clone()),
                _ => TypeWrapper::Concrete(AbsType::Dyn()),
            };

            type_check_(state, env.clone(), strict, e, exp.clone())?;

            // TODO move this up once lets are rec
            env.insert(x.clone(), exp);
            type_check_(state, env, strict, t, ty)
        }
        Term::App(e, t) => {
            let src = TypeWrapper::Ptr(new_var(state.table));
            let arr = TypeWrapper::Concrete(AbsType::arrow(Box::new(src.clone()), Box::new(ty)));

            // This order shouldn't be changed, since applying a function to a record
            // may change how it's typed (static or dynamic)
            // This is good hint a bidirectional algorithm would make sense...
            type_check_(state, env.clone(), strict, e, arr)?;
            type_check_(state, env, strict, t, src)
        }
        Term::Var(x) => {
            let x_ty = env
                .get(&x)
                .ok_or_else(|| TypecheckError::UnboundIdentifier(x.clone(), pos.clone()))?;

            let instantiated =
                instantiate_foralls_with(&mut state.table, x_ty.clone(), TypeWrapper::Ptr);
            unify(state, env, strict, ty, instantiated)
        }
        Term::Enum(id) => {
            let row = TypeWrapper::Ptr(new_var(&mut state.table));
            // Do we really need to constraint on enums?
            // What's the meaning of this?
            // FIXME: change error when constraint failing.
            constraint(state, row.clone(), id.clone()).map_err(|_| TypecheckError::Sink())?;
            unify(
                state,
                env.clone(),
                strict,
                ty,
                TypeWrapper::Concrete(AbsType::Enum(Box::new(TypeWrapper::Concrete(
                    AbsType::RowExtend(id.clone(), None, Box::new(row)),
                )))),
            )
        }
        Term::Record(stat_map) => {
            let root_ty = if let TypeWrapper::Ptr(p) = ty {
                get_root(state.table, p)
            } else {
                ty.clone()
            };

            if let TypeWrapper::Concrete(AbsType::DynRecord(rec_ty)) = root_ty.clone() {
                // Checking for an dynamic record
                stat_map
                    .into_iter()
                    .try_for_each(|e| -> Result<(), TypecheckError> {
                        let (_, t) = e;
                        type_check_(state, env.clone(), strict, t, (*rec_ty).clone())
                    })
            } else {
                // inferring static record
                let row = stat_map.into_iter().try_fold(
                    TypeWrapper::Concrete(AbsType::RowEmpty()),
                    |acc, e| -> Result<TypeWrapper, TypecheckError> {
                        let (id, t) = e;

                        let ty = TypeWrapper::Ptr(new_var(state.table));
                        type_check_(state, env.clone(), strict, t, ty.clone())?;

                        //FIXME: return a proper error. Constraint failing.
                        constraint(state, acc.clone(), id.clone())
                            .map_err(|_| TypecheckError::Sink())?;

                        Ok(TypeWrapper::Concrete(AbsType::RowExtend(
                            id.clone(),
                            Some(Box::new(ty)),
                            Box::new(acc),
                        )))
                    },
                )?;

                unify(
                    state,
                    env,
                    strict,
                    ty,
                    TypeWrapper::Concrete(AbsType::StaticRecord(Box::new(row))),
                )
            }
        }
        Term::Op1(op, t) => {
            let ty_op = get_uop_type(state, env.clone(), strict, op)?;

            let src = TypeWrapper::Ptr(new_var(state.table));
            let arr = TypeWrapper::Concrete(AbsType::arrow(Box::new(src.clone()), Box::new(ty)));

            unify(state, env.clone(), strict, arr, ty_op)?;
            type_check_(state, env.clone(), strict, t, src)
        }
        Term::Op2(op, e, t) => {
            let ty_op = get_bop_type(state, env.clone(), strict, op)?;

            let src1 = TypeWrapper::Ptr(new_var(state.table));
            let src2 = TypeWrapper::Ptr(new_var(state.table));
            let arr = TypeWrapper::Concrete(AbsType::arrow(
                Box::new(src1.clone()),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(src2.clone()),
                    Box::new(ty),
                ))),
            ));

            unify(state, env.clone(), strict, arr, ty_op)?;
            type_check_(state, env.clone(), strict, e, src1)?;
            type_check_(state, env, strict, t, src2)
        }
        Term::Promise(ty2, _, t) => {
            let tyw2 = to_typewrapper(ty2.clone());

            let instantiated = instantiate_foralls_with(state.table, tyw2, TypeWrapper::Constant);

            unify(
                state,
                env.clone(),
                strict,
                ty.clone(),
                to_typewrapper(ty2.clone()),
            )?;
            type_check_(state, env, true, t, instantiated)
        }
        Term::Assume(ty2, _, t) => {
            unify(
                state,
                env.clone(),
                strict,
                ty.clone(),
                to_typewrapper(ty2.clone()),
            )?;
            let new_ty = TypeWrapper::Ptr(new_var(state.table));
            type_check_(state, env, false, t, new_ty)
        }
        Term::Sym(_) => unify(
            state,
            env,
            strict,
            ty,
            TypeWrapper::Concrete(AbsType::Sym()),
        ),
        Term::Wrapped(_, t)
        | Term::DefaultValue(t)
        | Term::ContractWithDefault(_, _, t)
        | Term::Docstring(_, t) => type_check_(state, env, strict, t, ty),
        Term::Contract(_, _) => Ok(()),
        Term::Import(_) => unify(
            state,
            env,
            strict,
            ty,
            TypeWrapper::Concrete(AbsType::Dyn()),
        ),
        Term::ResolvedImport(file_id) => {
            let t = state
                .resolver
                .get(file_id.clone())
                .expect("Internal error: resolved import not found ({:?}) during typechecking.");
            type_check(&t, state.resolver).map(|_ty| ())
        }
    }
}

/// The types on which the unification algorithm operates, which may be either a concrete type, a
/// type constant or a unification variable.
#[derive(Clone, PartialEq, Debug)]
pub enum TypeWrapper {
    /// A concrete type (like `Num` or `Str -> Str`).
    Concrete(AbsType<Box<TypeWrapper>>),
    /// A rigid type constant which cannot be unified with anything but itself.
    Constant(usize),
    /// A unification variable.
    Ptr(usize),
}

impl TypeWrapper {
    pub fn subst(self, id: Ident, to: TypeWrapper) -> TypeWrapper {
        use self::TypeWrapper::*;
        match self {
            Concrete(AbsType::Var(ref i)) if *i == id => to,
            Concrete(AbsType::Var(i)) => Concrete(AbsType::Var(i)),

            Concrete(AbsType::Forall(i, t)) => {
                if i == id {
                    Concrete(AbsType::Forall(i, t))
                } else {
                    let tt = *t;
                    Concrete(AbsType::Forall(i, Box::new(tt.subst(id, to))))
                }
            }
            // Trivial recursion
            Concrete(AbsType::Dyn()) => Concrete(AbsType::Dyn()),
            Concrete(AbsType::Num()) => Concrete(AbsType::Num()),
            Concrete(AbsType::Bool()) => Concrete(AbsType::Bool()),
            Concrete(AbsType::Str()) => Concrete(AbsType::Str()),
            Concrete(AbsType::Sym()) => Concrete(AbsType::Sym()),
            Concrete(AbsType::Flat(t)) => Concrete(AbsType::Flat(t)),
            Concrete(AbsType::Arrow(s, t)) => {
                let fs = s.subst(id.clone(), to.clone());
                let ft = t.subst(id, to);

                Concrete(AbsType::Arrow(Box::new(fs), Box::new(ft)))
            }
            Concrete(AbsType::RowEmpty()) => Concrete(AbsType::RowEmpty()),
            Concrete(AbsType::RowExtend(tag, ty, rest)) => Concrete(AbsType::RowExtend(
                tag,
                ty.map(|x| Box::new(x.subst(id.clone(), to.clone()))),
                Box::new(rest.subst(id, to)),
            )),
            Concrete(AbsType::Enum(row)) => Concrete(AbsType::Enum(Box::new(row.subst(id, to)))),
            Concrete(AbsType::StaticRecord(row)) => {
                Concrete(AbsType::StaticRecord(Box::new(row.subst(id, to))))
            }
            Concrete(AbsType::DynRecord(def_ty)) => {
                Concrete(AbsType::DynRecord(Box::new(def_ty.subst(id, to))))
            }
            Concrete(AbsType::List()) => Concrete(AbsType::List()),
            Constant(x) => Constant(x),
            Ptr(x) => Ptr(x),
        }
    }
}

/// Look for a binding in a row, or add a new one if it is not present and if allowed by [row
/// constraints](type.GConstr.html).
///
/// The row may be given as a concrete type or as a unification variable.
///
/// # Return
///
/// The type newly bound to `id` in the row together with the tail of the new row. If `id` was
/// already in `r`, it does not change the binding and return the corresponding type instead as a
/// first component.
fn row_add(
    state: &mut State,
    id: Ident,
    ty: Option<Box<TypeWrapper>>,
    mut r: TypeWrapper,
) -> Result<(Option<Box<TypeWrapper>>, TypeWrapper), RowUnifError> {
    if let TypeWrapper::Ptr(p) = r {
        r = get_root(state.table, p);
    }
    match r {
        TypeWrapper::Concrete(AbsType::RowEmpty()) => Err(RowUnifError::MissingRow()),
        TypeWrapper::Concrete(AbsType::RowExtend(id2, ty2, r2)) => {
            if id == id2 {
                Ok((ty2, *r2))
            } else {
                let (extracted_type, subrow) = row_add(state, id, ty, *r2)?;
                Ok((
                    extracted_type,
                    TypeWrapper::Concrete(AbsType::RowExtend(id2, ty2, Box::new(subrow))),
                ))
            }
        }
        TypeWrapper::Ptr(root) => {
            if let Some(set) = state.constr.get(&root) {
                if set.contains(&id) {
                    return Err(RowUnifError::IncompatibleConstraints());
                }
            }
            let new_row = TypeWrapper::Ptr(new_var(state.table));
            constraint(state, new_row.clone(), id.clone())?;
            state.table.insert(
                root,
                Some(TypeWrapper::Concrete(AbsType::RowExtend(
                    id,
                    ty.clone(),
                    Box::new(new_row.clone()),
                ))),
            );
            Ok((ty, new_row))
        }
        other => Err(RowUnifError::IllformedRow(other)),
    }
}

/// Try to unify two types.
pub fn unify(
    state: &mut State,
    env: Environment,
    strict: bool,
    mut t1: TypeWrapper,
    mut t2: TypeWrapper,
) -> Result<(), TypecheckError> {
    if !strict {
        // TODO think whether this makes sense, without this we can't write the Y combinator
        return Ok(());
    }
    if let TypeWrapper::Ptr(pt1) = t1 {
        t1 = get_root(state.table, pt1);
    }
    if let TypeWrapper::Ptr(pt2) = t2 {
        t2 = get_root(state.table, pt2);
    }

    // t1 and t2 are roots of the type
    match (t1, t2) {
        (TypeWrapper::Concrete(s1), TypeWrapper::Concrete(s2)) => match (s1, s2) {
            (AbsType::Dyn(), AbsType::Dyn()) => Ok(()),
            (AbsType::Num(), AbsType::Num()) => Ok(()),
            (AbsType::Bool(), AbsType::Bool()) => Ok(()),
            (AbsType::Str(), AbsType::Str()) => Ok(()),
            (AbsType::List(), AbsType::List()) => Ok(()),
            (AbsType::Sym(), AbsType::Sym()) => Ok(()),
            (AbsType::Arrow(s1s, s1t), AbsType::Arrow(s2s, s2t)) => {
                unify(state, env.clone(), strict, *s1s, *s2s)?;
                unify(state, env, strict, *s1t, *s2t)
            }
            (AbsType::Flat(s), AbsType::Flat(t)) => {
                if let Term::Var(s) = s.clone().into() {
                    if let Term::Var(t) = t.clone().into() {
                        if s == t {
                            return Ok(());
                        }
                    }
                }
                //FIXME: proper error (flat type mismatch)
                Err(TypecheckError::TypeMismatch())
            } // Right now it only unifies equally named variables
            (AbsType::RowEmpty(), AbsType::RowEmpty()) => Ok(()),
            (AbsType::RowExtend(id, ty, t), r2 @ AbsType::RowExtend(_, _, _)) => {
                let (ty2, r2) = row_add(state, id, ty.clone(), TypeWrapper::Concrete(r2))
                    .map_err(|_| TypecheckError::Sink())?;

                match (ty, ty2) {
                    (None, None) => Ok(()),
                    (Some(ty), Some(ty2)) => unify(state, env.clone(), strict, *ty, *ty2),
                    _ => Err(TypecheckError::TypeMismatch()),
                }?;
                unify(state, env, strict, *t, r2)
            }
            (AbsType::Enum(r), AbsType::Enum(r2)) => unify(state, env, strict, *r, *r2),
            (AbsType::StaticRecord(r), AbsType::StaticRecord(r2)) => {
                unify(state, env, strict, *r, *r2)
            }
            (AbsType::DynRecord(t), AbsType::DynRecord(t2)) => unify(state, env, strict, *t, *t2),
            (AbsType::Var(ref i1), AbsType::Var(ref i2)) if i1 == i2 => Ok(()),
            (AbsType::Forall(i1, t1t), AbsType::Forall(i2, t2t)) => {
                // Very stupid (slow) implementation
                let constant_type = TypeWrapper::Constant(new_var(state.table));

                unify(
                    state,
                    env,
                    strict,
                    t1t.subst(i1, constant_type.clone()),
                    t2t.subst(i2, constant_type),
                )
            }
            //FIXME: proper error (general type mismatch)
            (_a, _b) => Err(TypecheckError::TypeMismatch()),
        },
        (TypeWrapper::Ptr(r1), TypeWrapper::Ptr(r2)) => {
            if r1 != r2 {
                let mut r1_constr = state.constr.remove(&r1).unwrap_or_default();
                let mut r2_constr = state.constr.remove(&r2).unwrap_or_default();
                state
                    .constr
                    .insert(r1, r1_constr.drain().chain(r2_constr.drain()).collect());

                state.table.insert(r1, Some(TypeWrapper::Ptr(r2)));
            }
            Ok(())
        }

        (TypeWrapper::Ptr(p), s @ TypeWrapper::Concrete(_))
        | (TypeWrapper::Ptr(p), s @ TypeWrapper::Constant(_))
        | (s @ TypeWrapper::Concrete(_), TypeWrapper::Ptr(p))
        | (s @ TypeWrapper::Constant(_), TypeWrapper::Ptr(p)) => {
            state.table.insert(p, Some(s));
            Ok(())
        }
        (TypeWrapper::Constant(i1), TypeWrapper::Constant(i2)) if i1 == i2 => Ok(()),
        //FIXME: proper error (general type mismatch)
        (_a, _b) => Err(TypecheckError::TypeMismatch()),
    }
}

/// Convert a vanilla Nickel type to a type wrapper.
fn to_typewrapper(t: Types) -> TypeWrapper {
    let Types(t2) = t;

    let t3 = t2.map(|x| Box::new(to_typewrapper(*x)));

    TypeWrapper::Concrete(t3)
}

/// Extract the concrete type (if any) corresponding to a type wrapper.
fn to_type(table: &GTypes, ty: TypeWrapper) -> Types {
    match ty {
        TypeWrapper::Ptr(p) => match get_root(table, p) {
            t @ TypeWrapper::Concrete(_) => to_type(table, t),
            _ => Types(AbsType::Dyn()),
        },
        TypeWrapper::Constant(_) => Types(AbsType::Dyn()),
        TypeWrapper::Concrete(t) => {
            let mapped = t.map(|btyp| Box::new(to_type(table, *btyp)));
            Types(mapped)
        }
    }
}

/// Instantiate the type variables which are quantified in head position with type constants.
///
/// For example, `forall a. forall b. a -> (forall c. b -> c)` is transformed to `cst1 -> (forall
/// c. cst2 -> c)` where `cst1` and `cst2` are fresh type constants.  This is used when
/// typechecking `forall`s: all quantified type variables in head position are replaced by rigid
/// type constants, and the term is then typechecked normally. As these constants cannot be unified
/// with anything, this forces all the occurrences of a type variable to be the same type.
fn instantiate_foralls_with<F>(table: &mut GTypes, mut ty: TypeWrapper, f: F) -> TypeWrapper
where
    F: Fn(usize) -> TypeWrapper,
{
    if let TypeWrapper::Ptr(p) = ty {
        ty = get_root(table, p);
    }

    while let TypeWrapper::Concrete(AbsType::Forall(id, forall_ty)) = ty {
        let var = f(new_var(table));
        ty = forall_ty.subst(id, var);
    }

    ty
}

/// Type of unary operations.
pub fn get_uop_type(
    state: &mut State,
    env: Environment,
    strict: bool,
    op: &UnaryOp<RichTerm>,
) -> Result<TypeWrapper, TypecheckError> {
    Ok(match op {
        // forall a. bool -> a -> a -> a
        UnaryOp::Ite() => {
            let branches = TypeWrapper::Ptr(new_var(state.table));

            TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Bool())),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(branches.clone()),
                    Box::new(TypeWrapper::Concrete(AbsType::arrow(
                        Box::new(branches.clone()),
                        Box::new(branches),
                    ))),
                ))),
            ))
        }
        // Num -> Bool
        UnaryOp::IsZero() => TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Num())),
            Box::new(TypeWrapper::Concrete(AbsType::Bool())),
        )),
        // forall a. a -> Bool
        UnaryOp::IsNum()
        | UnaryOp::IsBool()
        | UnaryOp::IsStr()
        | UnaryOp::IsFun()
        | UnaryOp::IsList() => {
            let inp = TypeWrapper::Ptr(new_var(state.table));

            TypeWrapper::Concrete(AbsType::arrow(
                Box::new(inp),
                Box::new(TypeWrapper::Concrete(AbsType::Bool())),
            ))
        }
        // forall a. Dyn -> a
        UnaryOp::Blame() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                Box::new(res),
            ))
        }
        // Dyn -> Bool
        UnaryOp::Pol() => TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            Box::new(TypeWrapper::Concrete(AbsType::Bool())),
        )),
        // forall rows. ( rows ) -> ( `id, rows )
        UnaryOp::Embed(id) => {
            let row = TypeWrapper::Ptr(new_var(state.table));
            //FIXME: proper error (constraint failed)
            constraint(state, row.clone(), id.clone()).map_err(|_| TypecheckError::Sink())?;
            TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Enum(Box::new(row.clone())))),
                Box::new(TypeWrapper::Concrete(AbsType::Enum(Box::new(
                    TypeWrapper::Concrete(AbsType::RowExtend(id.clone(), None, Box::new(row))),
                )))),
            ))
        }
        // 1. rows -> a
        // 2. forall b. b -> a
        // Rows is ( `label1, .., `labeln ) for label in l.keys().
        // Unify each branch in l.values() with a.
        // If the switch has a default case, the more general type 2. is used.
        UnaryOp::Switch(l, d) => {
            // Currently, if it has a default value, we typecheck the whole thing as
            // taking ANY enum, since it's more permissive and there's not a loss of information
            let res = TypeWrapper::Ptr(new_var(state.table));

            for exp in l.values() {
                type_check_(state, env.clone(), strict, exp, res.clone())?;
            }

            let row = match d {
                Some(e) => {
                    type_check_(state, env.clone(), strict, e, res.clone())?;
                    TypeWrapper::Ptr(new_var(state.table))
                }
                None => l.iter().try_fold(
                    TypeWrapper::Concrete(AbsType::RowEmpty()),
                    |acc, x| -> Result<TypeWrapper, TypecheckError> {
                        //FIXME: proper error (constraint failed)
                        constraint(state, acc.clone(), x.0.clone())
                            .map_err(|_| TypecheckError::Sink())?;
                        Ok(TypeWrapper::Concrete(AbsType::RowExtend(
                            x.0.clone(),
                            None,
                            Box::new(acc),
                        )))
                    },
                )?,
            };

            TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Enum(Box::new(row)))),
                Box::new(res),
            ))
        }
        // Dyn -> Dyn
        UnaryOp::ChangePolarity() | UnaryOp::GoDom() | UnaryOp::GoCodom() | UnaryOp::Tag(_) => {
            TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            ))
        }
        // Sym -> Dyn -> Dyn
        UnaryOp::Wrap() => TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Sym())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            ))),
        )),
        // forall rows a. { rows, id: a } -> a
        UnaryOp::StaticAccess(id) => {
            let row = TypeWrapper::Ptr(new_var(state.table));
            let res = TypeWrapper::Ptr(new_var(state.table));

            TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::StaticRecord(Box::new(
                    TypeWrapper::Concrete(AbsType::RowExtend(
                        id.clone(),
                        Some(Box::new(res.clone())),
                        Box::new(row),
                    )),
                )))),
                Box::new(res),
            ))
        }
        // { _ : a} -> { _ : b }
        // Unify f with Str -> a -> b.
        UnaryOp::MapRec(f) => {
            // Assuming f has type Str -> a -> b,
            // this has type DynRecord(a) -> DynRecord(b)

            let a = TypeWrapper::Ptr(new_var(state.table));
            let b = TypeWrapper::Ptr(new_var(state.table));

            let f_type = TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                    Box::new(a.clone()),
                    Box::new(b.clone()),
                ))),
            ));

            type_check_(state, env.clone(), strict, f, f_type)?;

            TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(a)))),
                Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(b)))),
            ))
        }
        // forall a b. a -> b -> b
        UnaryOp::Seq() | UnaryOp::DeepSeq() => {
            let fst = TypeWrapper::Ptr(new_var(state.table));
            let snd = TypeWrapper::Ptr(new_var(state.table));

            TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(fst),
                Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                    Box::new(snd.clone()),
                    Box::new(snd),
                ))),
            ))
        }
        // List -> Dyn
        UnaryOp::ListHead() => TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::List())),
            Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
        )),
        // List -> List
        UnaryOp::ListTail() => TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::List())),
            Box::new(TypeWrapper::Concrete(AbsType::List())),
        )),
        // List -> Num
        UnaryOp::ListLength() => TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::List())),
            Box::new(TypeWrapper::Concrete(AbsType::Num())),
        )),
        // This should not happen, as ChunksConcat() is only produced during evaluation.
        UnaryOp::ChunksConcat(_, _) => panic!("cannot type ChunksConcat()"),
    })
}

/// Type of a binary operation.
pub fn get_bop_type(
    state: &mut State,
    env: Environment,
    strict: bool,
    op: &BinaryOp<RichTerm>,
) -> Result<TypeWrapper, TypecheckError> {
    match op {
        // Num -> Num -> Num
        BinaryOp::Plus() => Ok(TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Num())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Num())),
                Box::new(TypeWrapper::Concrete(AbsType::Num())),
            ))),
        ))),
        // Str -> Str -> Str
        BinaryOp::PlusStr() => Ok(TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Str())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
            ))),
        ))),
        // Sym -> Dyn -> Dyn -> Dyn
        BinaryOp::Unwrap() => Ok(TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Sym())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                    Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                ))),
            ))),
        ))),
        // Bool -> Bool -> Bool
        BinaryOp::EqBool() => Ok(TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Bool())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Bool())),
                Box::new(TypeWrapper::Concrete(AbsType::Bool())),
            ))),
        ))),
        // forall a. Str -> { _ : a} -> a
        BinaryOp::DynAccess() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            Ok(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(
                        res.clone(),
                    )))),
                    Box::new(res),
                ))),
            )))
        }
        // Str -> { _ : a } -> { _ : a }
        // Unify t with a.
        BinaryOp::DynExtend(t) => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            type_check_(state, env.clone(), strict, t, res.clone())?;

            Ok(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(
                        res.clone(),
                    )))),
                    Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(
                        res.clone(),
                    )))),
                ))),
            )))
        }
        // forall a. Str -> { _ : a } -> { _ : a}
        BinaryOp::DynRemove() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            Ok(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::arrow(
                    Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(
                        res.clone(),
                    )))),
                    Box::new(TypeWrapper::Concrete(AbsType::DynRecord(Box::new(
                        res.clone(),
                    )))),
                ))),
            )))
        }
        // Str -> Dyn -> Bool
        BinaryOp::HasField() => Ok(TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Str())),
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            ))),
            Box::new(TypeWrapper::Concrete(AbsType::Bool())),
        ))),
        // List -> List -> List
        BinaryOp::ListConcat() => Ok(TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::List())),
            Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::List())),
                Box::new(TypeWrapper::Concrete(AbsType::List())),
            ))),
        ))),
        // forall a b. (a -> b) -> List -> List
        BinaryOp::ListMap() => {
            let src = TypeWrapper::Ptr(new_var(state.table));
            let tgt = TypeWrapper::Ptr(new_var(state.table));
            let arrow = TypeWrapper::Concrete(AbsType::Arrow(Box::new(src), Box::new(tgt)));

            Ok(TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(arrow),
                Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                    Box::new(TypeWrapper::Concrete(AbsType::List())),
                    Box::new(TypeWrapper::Concrete(AbsType::List())),
                ))),
            )))
        }
        // List -> Num -> Dyn
        BinaryOp::ListElemAt() => Ok(TypeWrapper::Concrete(AbsType::Arrow(
            Box::new(TypeWrapper::Concrete(AbsType::List())),
            Box::new(TypeWrapper::Concrete(AbsType::Arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Num())),
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            ))),
        ))),
        // Dyn -> Dyn -> Dyn
        BinaryOp::Merge() => Ok(TypeWrapper::Concrete(AbsType::arrow(
            Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            Box::new(TypeWrapper::Concrete(AbsType::arrow(
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
                Box::new(TypeWrapper::Concrete(AbsType::Dyn())),
            ))),
        ))),
    }
}

/// The unification table.
///
/// Map each unification variable to either another type variable or a concrete type it has been
/// unified with. Each binding `(ty, var)` in this map should be thought of an edge in a
/// unification graph.
pub type GTypes = HashMap<usize, Option<TypeWrapper>>;

/// Row constraints.
///
/// A row constraint applies to a unification variable appearing inside a row type (such as `r` in
/// `{ someId: SomeType, r }`). It is a set of identifiers that said row must NOT contain, to
/// forbid ill-formed types with multiple declaration of the same id, for example `{ a: Num, a:
/// String}`.
pub type GConstr = HashMap<usize, HashSet<Ident>>;

/// Create a fresh unification variable.
fn new_var(state: &mut GTypes) -> usize {
    let nxt = state.len();
    state.insert(nxt, None);
    nxt
}

/// Add a row constraint on a type.
///
/// See [`GConstr`](type.GConstr.html).
fn constraint(state: &mut State, x: TypeWrapper, id: Ident) -> Result<(), RowUnifError> {
    match x {
        TypeWrapper::Ptr(p) => match get_root(state.table, p) {
            ty @ TypeWrapper::Concrete(_) => constraint(state, ty, id),
            TypeWrapper::Ptr(root) => {
                if let Some(v) = state.constr.get_mut(&root) {
                    v.insert(id);
                } else {
                    state.constr.insert(root, vec![id].into_iter().collect());
                }
                Ok(())
            }
            c @ TypeWrapper::Constant(_) => Err(RowUnifError::IllformedRow(c)),
        },
        TypeWrapper::Concrete(AbsType::RowEmpty()) => Ok(()),
        TypeWrapper::Concrete(AbsType::RowExtend(id2, _, t)) => {
            if id2 == id {
                Err(RowUnifError::ConstraintFailed(id))
            } else {
                constraint(state, *t, id)
            }
        }
        other => Err(RowUnifError::IllformedRow(other)),
    }
}

/// Follow the links in the unification table to find the representative of the equivalence class
/// of unification variable `x`.
///
/// This corresponds to the find in union-find.
// TODO This should be a union find like algorithm
pub fn get_root(table: &GTypes, x: usize) -> TypeWrapper {
    match table.get(&x).unwrap() {
        None => TypeWrapper::Ptr(x),
        Some(TypeWrapper::Ptr(y)) => get_root(table, *y),
        Some(ty @ TypeWrapper::Concrete(_)) => ty.clone(),
        Some(k @ TypeWrapper::Constant(_)) => k.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ImportError;
    use crate::label::Label;
    use crate::parser::lexer;
    use crate::program::resolvers::{DummyResolver, SimpleResolver};
    use crate::transformations::transform;
    use codespan::Files;

    use crate::parser;

    fn type_check_no_import(rt: &RichTerm) -> Result<Types, TypecheckError> {
        type_check(rt, &mut DummyResolver {})
    }

    fn parse_and_typecheck(s: &str) -> Result<Types, TypecheckError> {
        let id = Files::new().add("<test>", s);

        if let Ok(p) = parser::grammar::TermParser::new().parse(id, lexer::Lexer::new(&s)) {
            type_check_no_import(&p)
        } else {
            panic!("Couldn't parse {}", s)
        }
    }

    #[test]
    fn simple_no_promises() -> Result<(), TypecheckError> {
        // It's easy to check these will never fail, that's why we keep them all together

        type_check_no_import(&Term::Bool(true).into())?;
        type_check_no_import(&Term::Num(45.).into())?;
        type_check_no_import(&RichTerm::fun(String::from("x"), RichTerm::var("x".into())).into())?;
        type_check_no_import(&RichTerm::let_in(
            "x",
            Term::Num(3.).into(),
            RichTerm::var("x".into()),
        ))?;

        type_check_no_import(&RichTerm::app(
            Term::Num(5.).into(),
            Term::Bool(true).into(),
        ))?;
        type_check_no_import(&RichTerm::plus(
            Term::Num(4.).into(),
            Term::Bool(false).into(),
        ))?;

        Ok(())
    }

    #[test]
    fn unbound_variable_always_throws() {
        type_check_no_import(&RichTerm::var(String::from("x"))).unwrap_err();
    }

    #[test]
    fn promise_simple_checks() {
        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Bool()),
                Label::dummy(),
                Term::Bool(true).into(),
            )
            .into(),
        )
        .unwrap();
        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Num()),
                Label::dummy(),
                Term::Bool(true).into(),
            )
            .into(),
        )
        .unwrap_err();

        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Num()),
                Label::dummy(),
                Term::Num(34.5).into(),
            )
            .into(),
        )
        .unwrap();
        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Bool()),
                Label::dummy(),
                Term::Num(34.5).into(),
            )
            .into(),
        )
        .unwrap_err();

        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Num()),
                Label::dummy(),
                Term::Assume(
                    Types(AbsType::Num()),
                    Label::dummy(),
                    Term::Bool(true).into(),
                )
                .into(),
            )
            .into(),
        )
        .unwrap();
        type_check_no_import(
            &Term::Promise(
                Types(AbsType::Num()),
                Label::dummy(),
                Term::Assume(
                    Types(AbsType::Bool()),
                    Label::dummy(),
                    Term::Num(34.).into(),
                )
                .into(),
            )
            .into(),
        )
        .unwrap_err();

        parse_and_typecheck("Promise(Str, \"hello\")").unwrap();
        parse_and_typecheck("Promise(Num, \"hello\")").unwrap_err();
    }

    #[test]
    fn promise_complicated() {
        // Inside Promises we typecheck strictly
        parse_and_typecheck("(fun x => if x then x + 1 else 34) false").unwrap();
        parse_and_typecheck("Promise(Bool -> Num, fun x => if x then x + 1 else 34) false")
            .unwrap_err();

        // not annotated let bindings type to Dyn
        parse_and_typecheck(
            "let id = Promise(Num -> Num, fun x => x) in
            Promise(Num, id 4)",
        )
        .unwrap();
        parse_and_typecheck(
            "let id = fun x => x in
            Promise(Num, id 4)",
        )
        .unwrap_err();

        // lambdas don't annotate to Dyn
        parse_and_typecheck("(fun id => Promise(Num, id 4)) (fun x => x)").unwrap();

        // But they are not polymorphic
        parse_and_typecheck("(fun id => Promise(Num, id 4) + Promise(Bool, id true)) (fun x => x)")
            .unwrap_err();

        // Non strict zones don't unify
        parse_and_typecheck("(fun id => (id 4) + Promise(Bool, id true)) (fun x => x)").unwrap();

        // We can typecheck any contract
        parse_and_typecheck(
            "let alwaysTrue = fun l t => if t then t else blame l in
        Promise(#alwaysTrue -> #alwaysTrue, fun x => x)",
        )
        .unwrap();
        // Only if they're named the same way
        parse_and_typecheck("Promise(#(fun l t => t) -> #(fun l t => t), fun x => x)").unwrap_err();
    }

    #[test]
    fn simple_forall() {
        parse_and_typecheck(
            "let f = Promise(forall a. a -> a, fun x => x) in
        Promise(Num, if (f true) then (f 2) else 3)",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(forall a. (forall b. a -> b -> a), fun x y => x) in
        Promise(Num, if (f true 3) then (f 2 false) else 3)",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(forall a. (forall b. b -> b) -> a -> a, fun f x => f x) in
            f Promise(forall y. y -> y, fun z => z)",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(forall a. (forall b. a -> b -> a), fun x y => y) in
            f",
        )
        .unwrap_err();

        parse_and_typecheck(
            "Promise(
                ((forall a. a -> a) -> Num) -> Num,
                fun f => let g = Promise(forall b. b -> b, fun y => y) in f g)
            (fun x => 3)",
        )
        .unwrap_err();

        parse_and_typecheck(
            "let g = Promise(Num -> Num, fun x => x) in
        let f = Promise(forall a. a -> a, fun x =>  g x) in
        f",
        )
        .unwrap_err();
    }

    #[test]
    fn forall_nested() {
        parse_and_typecheck(
            "let f = Promise(forall a. a -> a, let g = Assume(forall a. (a -> a), fun x => x) in g) in
            Promise(Num, if (f true) then (f 2) else 3)",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(forall a. a -> a, let g = Promise(forall a. (a -> a), fun x => x) in g g) in
            Promise(Num, if (f true) then (f 2) else 3)",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(forall a. a -> a, let g = Promise(forall a. (forall b. (b -> (a -> a))), fun y x => x) in g 0) in
            Promise(Num, if (f true) then (f 2) else 3)",
        )
        .unwrap();
    }

    #[test]
    fn enum_simple() {
        parse_and_typecheck("Promise(< (| bla, |) >, `bla)").unwrap();
        parse_and_typecheck("Promise(< (| bla, |) >, `blo)").unwrap_err();

        parse_and_typecheck("Promise(< (| bla, blo, |) >, `blo)").unwrap();
        parse_and_typecheck("Promise(forall r. < (| bla, | r ) >, `bla)").unwrap();
        parse_and_typecheck("Promise(forall r. < (| bla, blo, | r ) >, `bla)").unwrap();

        parse_and_typecheck("Promise(Num, switch { bla => 3, } `bla)").unwrap();
        parse_and_typecheck("Promise(Num, switch { bla => 3, } `blo)").unwrap_err();

        parse_and_typecheck("Promise(Num, switch { bla => 3, _ => 2, } `blo)").unwrap();
        parse_and_typecheck("Promise(Num, switch { bla => 3, ble => true, } `bla)").unwrap_err();
    }

    #[test]
    fn enum_complex() {
        parse_and_typecheck(
            "Promise(< (| bla, ble, |) > -> Num, fun x => switch {bla => 1, ble => 2,} x)",
        )
        .unwrap();
        parse_and_typecheck(
            "Promise(< (| bla, ble, |) > -> Num,
        fun x => switch {bla => 1, ble => 2, bli => 4,} x)",
        )
        .unwrap_err();
        parse_and_typecheck(
            "Promise(< (| bla, ble, |) > -> Num,
        fun x => switch {bla => 1, ble => 2, bli => 4,} (embed bli x))",
        )
        .unwrap();

        parse_and_typecheck(
            "Promise(Num, 
            (fun x => 
                (switch {bla => 3, bli => 2,} x) + 
                (switch {bli => 6, bla => 20,} x) ) `bla)",
        )
        .unwrap();
        // TODO typecheck this, I'm not sure how to do it with row variables
        parse_and_typecheck(
            "Promise(Num, 
            (fun x => 
                (switch {bla => 3, bli => 2,} x) + 
                (switch {bla => 6, blo => 20,} x) ) `bla)",
        )
        .unwrap_err();

        parse_and_typecheck(
            "let f = Promise(
                forall r. < (| blo, ble, | r )> -> Num,
                fun x => (switch {blo => 1, ble => 2, _ => 3, } x ) ) in
            Promise(Num, f `bli)",
        )
        .unwrap();
        parse_and_typecheck(
            "let f = Promise(
                forall r. < (| blo, ble, | r )> -> Num,
                fun x => (switch {blo => 1, ble => 2, bli => 3, } x ) ) in
            f",
        )
        .unwrap_err();

        parse_and_typecheck(
            "let f = Promise(
                forall r. (forall p. < (| blo, ble, | r )> -> < (| bla, bli, | p) > ),
                fun x => (switch {blo => `bla, ble => `bli, _ => `bla, } x ) ) in
            f `bli",
        )
        .unwrap();
        parse_and_typecheck(
            "let f = Promise(
                forall r. (forall p. < (| blo, ble, | r )> -> < (| bla, bli, | p) > ),
                fun x => (switch {blo => `bla, ble => `bli, _ => `blo, } x ) ) in
            f `bli",
        )
        .unwrap_err();
    }

    #[test]
    fn static_record_simple() {
        parse_and_typecheck("Promise({ {| bla : Num, |} }, { bla = 1; })").unwrap();
        parse_and_typecheck("Promise({ {| bla : Num, |} }, { bla = true; })").unwrap_err();
        parse_and_typecheck("Promise({ {| bla : Num, |} }, { blo = 1; })").unwrap_err();

        parse_and_typecheck("Promise({ {| bla : Num, blo : Bool, |} }, { blo = true; bla = 1; })")
            .unwrap();

        parse_and_typecheck("Promise(Num, { blo = 1; }.blo)").unwrap();
        parse_and_typecheck("Promise(Num, { bla = true; blo = 1; }.blo)").unwrap();
        parse_and_typecheck("Promise(Bool, { blo = 1; }.blo)").unwrap_err();

        parse_and_typecheck(
            "let r = Promise({ {| bla : Bool, blo : Num, |} }, {blo = 1; bla = true; }) in
        Promise(Num, if r.bla then r.blo else 2)",
        )
        .unwrap();

        // It worked at first try :O
        parse_and_typecheck(
            "let f = Promise(
                forall a. (forall r. { {| bla : Bool, blo : a, ble : a, | r } } -> a),
                fun r => if r.bla then r.blo else r.ble) 
            in
            Promise(Num, 
                if (f {bla = true; blo = false; ble = true; blip = 1; }) then
                    (f {bla = true; blo = 1; ble = 2; blip = `blip; })
                else
                    (f {bla = true; blo = 3; ble = 4; bloppo = `bloppop; }))",
        )
        .unwrap();

        parse_and_typecheck(
            "let f = Promise(
                forall a. (forall r. { {| bla : Bool, blo : a, ble : a, | r } } -> a),
                fun r => if r.bla then r.blo else r.ble) 
            in
            Promise(Num, 
                    f {bla = true; blo = 1; ble = true; blip = `blip; })
                ",
        )
        .unwrap_err();
        parse_and_typecheck(
            "let f = Promise(
                forall a. (forall r. { {| bla : Bool, blo : a, ble : a, | r } } -> a),
                fun r => if r.bla then (r.blo + 1) else r.ble) 
            in
            Promise(Num, 
                    f {bla = true; blo = 1; ble = 2; blip = `blip; })
                ",
        )
        .unwrap_err();
    }

    #[test]
    fn dynamic_record_simple() {
        parse_and_typecheck("Promise({ _ : Num }, { $(if true then \"foo\" else \"bar\") = 2; } )")
            .unwrap();

        parse_and_typecheck(
            "Promise(Num, { $(if true then \"foo\" else \"bar\") = 2; }.$(\"bla\"))",
        )
        .unwrap();

        parse_and_typecheck(
            "Promise(
                Num, 
                { $(if true then \"foo\" else \"bar\") = 2; $(\"foo\") = true; }.$(\"bla\"))",
        )
        .unwrap_err();

        parse_and_typecheck("Promise( { _ : Num}, { foo = 3; bar = 4; })").unwrap();
    }

    #[test]
    fn seq() {
        parse_and_typecheck("Promise(Num, seq false 1)").unwrap();
        parse_and_typecheck("Promise(forall a. (forall b. a -> b -> b), fun x y => seq x y)")
            .unwrap();
        parse_and_typecheck("let xDyn = false in let yDyn = 1 in Promise(Dyn, seq xDyn yDyn)")
            .unwrap();
    }

    #[test]
    fn simple_list() {
        parse_and_typecheck("[1, \"2\", false]").unwrap();
        parse_and_typecheck("Promise(List, [\"a\", 3, true])").unwrap();
        parse_and_typecheck("Promise(List, [Promise(forall a. a -> a, fun x => x), 3, true])")
            .unwrap();
        parse_and_typecheck("Promise(forall a. a -> List, fun x => [x])").unwrap();

        parse_and_typecheck("[1, Promise(Num, \"2\"), false]").unwrap_err();
        parse_and_typecheck("Promise(List, [Promise(String,1), true, \"b\"])").unwrap_err();
        parse_and_typecheck("Promise(Num, [1, 2, \"3\"])").unwrap_err();
    }

    #[test]
    fn lists_operations() {
        parse_and_typecheck("Promise(List -> List, fun l => tail l)").unwrap();
        parse_and_typecheck("Promise(List -> Dyn, fun l => head l)").unwrap();
        parse_and_typecheck(
            "Promise(forall a. (forall b. (a -> b) -> List -> List), fun f l => map f l)",
        )
        .unwrap();
        parse_and_typecheck("Promise(List -> List -> List, fun l1 => fun l2 => l1 @ l2)").unwrap();
        parse_and_typecheck("Promise(Num -> List -> Dyn , fun i l => elemAt l i)").unwrap();

        parse_and_typecheck("Promise(forall a. (List -> a), fun l => head l)").unwrap_err();
        parse_and_typecheck(
            "Promise(forall a. (forall b. (a -> b) -> List -> b), fun f l => elemAt (map f l) 0)",
        )
        .unwrap_err();
    }

    #[test]
    fn imports() {
        let mut resolver = SimpleResolver::new();
        resolver.add_source(String::from("good"), String::from("Promise(Num, 1 + 1)"));
        resolver.add_source(String::from("bad"), String::from("Promise(Num, false)"));
        resolver.add_source(
            String::from("proxy"),
            String::from("let x = import \"bad\" in x"),
        );

        fn mk_import<R>(import: &str, resolver: &mut R) -> Result<RichTerm, ImportError>
        where
            R: ImportResolver,
        {
            transform(
                RichTerm::let_in(
                    "x",
                    Term::Import(String::from(import)).into(),
                    RichTerm::var(String::from("x")),
                ),
                resolver,
            )
        };

        type_check(&mk_import("good", &mut resolver).unwrap(), &mut resolver).unwrap();

        type_check(&mk_import("proxy", &mut resolver).unwrap(), &mut resolver).unwrap_err();
    }
}
