//! Implementation of primitive operations.
//!
//! Define functions which perform the evaluation of primitive operators. The machinery required
//! for the strict evaluation of the operands is mainly handled by [`eval`](../eval/index.html),
//! and marginally in [`continuate_operation`](fn.continuate_operation.html). On the other hand,
//! the functions [`process_unary_operation`](fn.process_unary_operation.html) and
//! [`process_binary_operation`](fn.process_binary_operation.html) receive evaluated operands and
//! implement the actual semantics of operators.
use crate::error::EvalError;
use crate::eval::Environment;
use crate::eval::{CallStack, Closure};
use crate::identifier::Ident;
use crate::label::ty_path;
use crate::merge;
use crate::merge::merge;
use crate::position::RawSpan;
use crate::stack::Stack;
use crate::stdlib;
use crate::term::make as mk_term;
use crate::term::{BinaryOp, RichTerm, StrChunk, Term, UnaryOp};
use crate::transformations::Closurizable;
use crate::{mk_app, mk_fun};
use simple_counter::*;
use std::collections::HashMap;

generate_counter!(FreshVariableCounter, usize);

/// An operation continuation as stored on the stack.
#[derive(Debug, PartialEq)]
pub enum OperationCont {
    Op1(
        /* unary operation */ UnaryOp<Closure>,
        /* original position of the argument before evaluation */ Option<RawSpan>,
    ),
    // The last parameter saves the strictness mode before the evaluation of the operator
    Op2First(
        /* the binary operation */ BinaryOp<Closure>,
        /* second argument, to evaluate next */ Closure,
        /* original position of the first argument */ Option<RawSpan>,
        /* previous value of enriched_strict */ bool,
    ),
    Op2Second(
        /* binary operation */ BinaryOp<Closure>,
        /* first argument, evaluated */ Closure,
        /* original position of the first argument before evaluation */ Option<RawSpan>,
        /* original position of the second argument before evaluation */ Option<RawSpan>,
        /* previous value of enriched_strict */ bool,
    ),
}

/// Process to the next step of the evaluation of an operation.
///
/// Depending on the content of the stack, it either starts the evaluation of the first argument,
/// starts the evaluation of the second argument, or finally process with the operation if both
/// arguments are evaluated (for binary operators).
pub fn continuate_operation(
    mut clos: Closure,
    stack: &mut Stack,
    call_stack: &mut CallStack,
    enriched_strict: &mut bool,
) -> Result<Closure, EvalError> {
    let (cont, cs_len, pos) = stack.pop_op_cont().expect("Condition already checked");
    call_stack.truncate(cs_len);
    match cont {
        OperationCont::Op1(u_op, arg_pos) => {
            process_unary_operation(u_op, clos, arg_pos, stack, pos)
        }
        OperationCont::Op2First(b_op, mut snd_clos, fst_pos, prev_strict) => {
            std::mem::swap(&mut clos, &mut snd_clos);
            stack.push_op_cont(
                OperationCont::Op2Second(
                    b_op,
                    snd_clos,
                    fst_pos,
                    clos.body.pos.clone(),
                    prev_strict,
                ),
                cs_len,
                pos,
            );
            Ok(clos)
        }
        OperationCont::Op2Second(b_op, fst_clos, fst_pos, snd_pos, prev_strict) => {
            let result =
                process_binary_operation(b_op, fst_clos, fst_pos, clos, snd_pos, stack, pos);
            *enriched_strict = prev_strict;
            result
        }
    }
}

/// Evaluate a unary operation.
///
/// The argument is expected to be evaluated (in WHNF). `pos_op` corresponds to the whole
/// operation position, that may be needed for error reporting.
fn process_unary_operation(
    u_op: UnaryOp<Closure>,
    clos: Closure,
    arg_pos: Option<RawSpan>,
    stack: &mut Stack,
    pos_op: Option<RawSpan>,
) -> Result<Closure, EvalError> {
    let Closure {
        body: RichTerm { term: t, pos },
        mut env,
    } = clos;
    match u_op {
        UnaryOp::Ite() => {
            if let Term::Bool(b) = *t {
                if stack.count_args() >= 2 {
                    let (fst, _) = stack.pop_arg().expect("Condition already checked.");
                    let (snd, _) = stack.pop_arg().expect("Condition already checked.");

                    Ok(if b { fst } else { snd })
                } else {
                    panic!("An If-Then-Else wasn't saturated")
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Bool"),
                    op: String::from("if"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::IsZero() => {
            if let Term::Num(n) = *t {
                // TODO Discuss and decide on this comparison for 0 on f64
                Ok(Closure::atomic_closure(Term::Bool(n == 0.).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("isZero"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::IsNum() => {
            if let Term::Num(_) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::IsBool() => {
            if let Term::Bool(_) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::IsStr() => {
            if let Term::Str(_) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::IsFun() => {
            if let Term::Fun(_, _) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::IsList() => {
            if let Term::List(_) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::IsRecord() => match *t {
            Term::Record(_) | Term::RecRecord(_) => {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            }
            _ => Ok(Closure::atomic_closure(Term::Bool(false).into())),
        },
        UnaryOp::BoolAnd() =>
        // The syntax should not allow partially applied boolean operators.
        {
            if let Some((next, _)) = stack.pop_arg() {
                match *t {
                    Term::Bool(true) => Ok(next),
                    // FIXME: this does not check that the second argument is actually a boolean.
                    // This means `true && 2` silently evaluates to `2`. This is simpler and more
                    // efficient, but can make debugging harder. In any case, it should be solved
                    // only once primary operators have better support for laziness in some
                    // arguments.
                    b @ Term::Bool(false) => Ok(Closure::atomic_closure(b.into())),
                    _ => Err(EvalError::TypeError {
                        expd: String::from("Bool"),
                        op: String::from("&&"),
                        t: RichTerm { term: t, pos },
                        pos: arg_pos,
                    }),
                }
            } else {
                Err(EvalError::NotEnoughArgs {
                    required: 2,
                    op: String::from("&&"),
                    pos: pos_op,
                })
            }
        }
        UnaryOp::BoolOr() => {
            if let Some((next, _)) = stack.pop_arg() {
                match *t {
                    b @ Term::Bool(true) => Ok(Closure::atomic_closure(b.into())),
                    // FIXME: this does not check that the second argument is actually a boolean.
                    // This means `false || 2` silently evaluates to `2`. This is simpler and more
                    // efficient, but can make debugging harder. In any case, it should be solved
                    // only once primary operators have better support for laziness in some
                    // arguments.
                    Term::Bool(false) => Ok(next),
                    _ => Err(EvalError::TypeError {
                        expd: String::from("Bool"),
                        op: String::from("||"),
                        t: RichTerm { term: t, pos },
                        pos: arg_pos,
                    }),
                }
            } else {
                Err(EvalError::NotEnoughArgs {
                    required: 2,
                    op: String::from("||"),
                    pos: pos_op,
                })
            }
        }
        UnaryOp::BoolNot() => {
            if let Term::Bool(b) = *t {
                Ok(Closure::atomic_closure(Term::Bool(!b).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Bool"),
                    op: String::from("!"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Blame() => {
            if let Term::Lbl(label) = *t {
                Err(EvalError::BlameError {
                    label,
                    call_stack: None,
                })
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("blame"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Embed(_id) => {
            if let en @ Term::Enum(_) = *t {
                Ok(Closure::atomic_closure(en.into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Enum"),
                    op: String::from("embed"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Switch(mut m, d) => {
            if let Term::Enum(en) = *t {
                match m.remove(&en) {
                    Some(clos) => Ok(clos),
                    None => match d {
                        Some(clos) => Ok(clos),
                        None => Err(EvalError::TypeError {
                            expd: String::from("Enum"),
                            op: String::from("switch"),
                            t: RichTerm {
                                term: Box::new(Term::Enum(en)),
                                pos,
                            },
                            pos: arg_pos,
                        }),
                    },
                }
            } else {
                match d {
                    Some(clos) => Ok(clos),
                    None => Err(EvalError::TypeError {
                        expd: String::from("Enum"),
                        op: String::from("switch"),
                        t: RichTerm { term: t, pos },
                        pos: arg_pos,
                    }),
                }
            }
        }
        UnaryOp::ChangePolarity() => {
            if let Term::Lbl(mut l) = *t {
                l.polarity = !l.polarity;
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("changePolarity"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Pol() => {
            if let Term::Lbl(l) = *t {
                Ok(Closure::atomic_closure(Term::Bool(l.polarity).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("polarity"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::GoDom() => {
            if let Term::Lbl(mut l) = *t {
                l.path.push(ty_path::Elem::Domain);
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("goDom"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::GoCodom() => {
            if let Term::Lbl(mut l) = *t {
                l.path.push(ty_path::Elem::Codomain);
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("goCodom"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Tag(s) => {
            if let Term::Lbl(mut l) = *t {
                l.tag = String::from(&s);
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Label"),
                    op: String::from("tag"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Wrap() => {
            if let Term::Sym(s) = *t {
                Ok(Closure::atomic_closure(mk_fun!(
                    "x",
                    Term::Wrapped(s, mk_term::var("x"))
                )))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Sym"),
                    op: String::from("wrap"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::StaticAccess(id) => {
            if let Term::Record(mut static_map) = *t {
                match static_map.remove(&id) {
                    Some(e) => Ok(Closure { body: e, env }),

                    None => Err(EvalError::FieldMissing {
                        field: format!("{}", id),
                        op: String::from("(.)"),
                        t: RichTerm {
                            term: Box::new(Term::Record(static_map)),
                            pos,
                        },
                        pos: pos_op,
                    }), //TODO include the position of operators on the stack
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Record"),
                    op: String::from("field access"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::FieldsOf() => {
            if let Term::Record(map) = *t {
                let mut fields: Vec<String> = map.keys().map(|Ident(id)| id.clone()).collect();
                fields.sort();
                let terms = fields.into_iter().map(mk_term::string).collect();
                Ok(Closure::atomic_closure(Term::List(terms).into()))
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Record"),
                    op: String::from("fieldsOf"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::MapRec(f) => {
            if let Term::Record(rec) = *t {
                let f_as_var = f.body.closurize(&mut env, f.env);

                let rec = rec
                    .into_iter()
                    .map(|e| {
                        let (Ident(s), t) = e;
                        (
                            Ident(s.clone()),
                            mk_app!(f_as_var.clone(), mk_term::string(s), t.clone()),
                        )
                    })
                    .collect();

                Ok(Closure {
                    body: Term::Record(rec).into(),
                    env,
                })
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Record"),
                    op: String::from("map on record"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::Seq() => {
            if stack.count_args() >= 1 {
                let (next, _) = stack.pop_arg().expect("Condition already checked.");
                Ok(next)
            } else {
                Err(EvalError::NotEnoughArgs {
                    required: 2,
                    op: String::from("seq"),
                    pos: pos_op,
                })
            }
        }
        UnaryOp::DeepSeq() => {
            /// Build a closure that forces a given list of terms, and at the end resumes the
            /// evaluation of the argument on the top of the stack.
            ///
            /// Requires its first argument to be non-empty.
            fn seq_terms<I>(mut terms: I, env: Environment) -> Result<Closure, EvalError>
            where
                I: Iterator<Item = RichTerm>,
            {
                let first = terms
                    .next()
                    .expect("expected the argument to be a non-empty iterator");
                let body = terms.fold(mk_term::op1(UnaryOp::DeepSeq(), first), |acc, t| {
                    mk_app!(mk_term::op1(UnaryOp::DeepSeq(), t), acc).into()
                });

                Ok(Closure { body, env })
            };

            match *t {
                Term::Record(map) if !map.is_empty() => {
                    let terms = map.into_iter().map(|(_, t)| t);
                    seq_terms(terms, env)
                }
                Term::List(ts) if !ts.is_empty() => seq_terms(ts.into_iter(), env),
                _ => {
                    if stack.count_args() >= 1 {
                        let (next, _) = stack.pop_arg().expect("Condition already checked.");
                        Ok(next)
                    } else {
                        Err(EvalError::NotEnoughArgs {
                            required: 2,
                            op: String::from("deepSeq"),
                            pos: pos_op,
                        })
                    }
                }
            }
        }
        UnaryOp::ListHead() => {
            if let Term::List(ts) = *t {
                let mut ts_it = ts.into_iter();
                if let Some(head) = ts_it.next() {
                    Ok(Closure { body: head, env })
                } else {
                    Err(EvalError::Other {
                        msg: String::from("head: empty list"),
                        pos: pos_op,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("List"),
                    op: String::from("head"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::ListTail() => {
            if let Term::List(ts) = *t {
                let mut ts_it = ts.into_iter();
                if let Some(_) = ts_it.next() {
                    Ok(Closure {
                        body: Term::List(ts_it.collect()).into(),
                        env,
                    })
                } else {
                    Err(EvalError::Other {
                        msg: String::from("tail: empty list"),
                        pos: pos_op,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("List"),
                    op: String::from("tail"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::ListLength() => {
            if let Term::List(ts) = *t {
                // A num does not have any free variable so we can drop the environment
                Ok(Closure {
                    body: Term::Num(ts.len() as f64).into(),
                    env: HashMap::new(),
                })
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("List"),
                    op: String::from("length"),
                    t: RichTerm { term: t, pos },
                    pos: arg_pos,
                })
            }
        }
        UnaryOp::ChunksConcat(mut acc, mut tail) => {
            if let Term::Str(s) = *t {
                acc.push_str(&s);
                let mut next_opt = tail.pop();

                // Pop consecutive string literals to find the next expression to evaluate
                while let Some(StrChunk::Literal(s)) = next_opt {
                    acc.push_str(&s);
                    next_opt = tail.pop();
                }

                if let Some(StrChunk::Expr(e)) = next_opt {
                    let arg_closure = e.body.closurize(&mut env, e.env);
                    let tail_closure = tail
                        .into_iter()
                        .map(|chunk| match chunk {
                            StrChunk::Literal(s) => StrChunk::Literal(s),
                            StrChunk::Expr(c) => StrChunk::Expr(c.body.closurize(&mut env, c.env)),
                        })
                        .collect();

                    Ok(Closure {
                        body: RichTerm {
                            term: Box::new(Term::Op1(
                                UnaryOp::ChunksConcat(acc, tail_closure),
                                arg_closure,
                            )),
                            pos: pos_op,
                        },
                        env,
                    })
                } else {
                    Ok(Closure {
                        body: RichTerm {
                            term: Box::new(Term::Str(acc)),
                            pos: pos_op,
                        },
                        env: HashMap::new(),
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("String"),
                    op: String::from("interpolated string"),
                    t: RichTerm { term: t, pos },
                    pos: pos_op,
                })
            }
        }
    }
}

/// Evaluate a binary operation.
///
/// Both arguments are expected to be evaluated (in WHNF). `pos_op` corresponds to the whole
/// operation position, that may be needed for error reporting.
fn process_binary_operation(
    b_op: BinaryOp<Closure>,
    fst_clos: Closure,
    fst_pos: Option<RawSpan>,
    clos: Closure,
    snd_pos: Option<RawSpan>,
    _stack: &mut Stack,
    pos_op: Option<RawSpan>,
) -> Result<Closure, EvalError> {
    let Closure {
        body: RichTerm {
            term: t1,
            pos: pos1,
        },
        env: env1,
    } = fst_clos;
    let Closure {
        body: RichTerm {
            term: t2,
            pos: pos2,
        },
        env: mut env2,
    } = clos;

    match b_op {
        BinaryOp::Plus() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Num(n1 + n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("+, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("+, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Sub() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Num(n1 - n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("-, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("-, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Mult() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Num(n1 * n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("*, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("*, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Div() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    if n2 == 0.0 {
                        Err(EvalError::Other {
                            msg: String::from("division by zero"),
                            pos: pos_op,
                        })
                    } else {
                        Ok(Closure::atomic_closure(Term::Num(n1 / n2).into()))
                    }
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("/, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("/, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Modulo() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Num(n1 % n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("%, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("%, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::PlusStr() => {
            if let Term::Str(s1) = *t1 {
                if let Term::Str(s2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Str(s1 + &s2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Str"),
                        op: String::from("++, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from("++, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Unwrap() => {
            if let Term::Sym(s1) = *t1 {
                // Return a function that either behaves like the identity or
                // const unwrapped_term

                Ok(if let Term::Wrapped(s2, t) = *t2 {
                    if s1 == s2 {
                        Closure {
                            body: mk_fun!("-invld", t),
                            env: env2,
                        }
                    } else {
                        Closure::atomic_closure(mk_term::id())
                    }
                } else {
                    Closure::atomic_closure(mk_term::id())
                })
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Sym"),
                    op: String::from("unwrap, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::Eq() => {
            /// Take an iterator of pairs of RichTerm, the common environments of all left
            /// components of these pairs and all right components, the final environment,
            /// and build a Term which evaluates to `Bool(true)` if and only if all the pairs are
            /// equals
            fn eq_all<T>(
                it: T,
                env1: &Environment,
                env2: &Environment,
                env: &mut Environment,
            ) -> Term
            where
                T: Iterator<Item = (RichTerm, RichTerm)>,
            {
                let subeqs: Vec<RichTerm> = it
                    .map(|(t1, t2)| {
                        let t1_var = t1.closurize(env, env1.clone());
                        let t2_var = t2.closurize(env, env2.clone());
                        Term::Op2(BinaryOp::Eq(), t1_var, t2_var).into()
                    })
                    .collect();
                // lists.all (fun x => x) subeqs
                Term::App(
                    mk_app!(stdlib::lists::all(), mk_term::id()),
                    Term::List(subeqs).into(),
                )
            }

            let mut env: Environment = HashMap::new();
            let res = match (*t1, *t2) {
                (Term::Bool(b1), Term::Bool(b2)) => Term::Bool(b1 == b2),
                (Term::Num(n1), Term::Num(n2)) => Term::Bool(n1 == n2),
                (Term::Str(s1), Term::Str(s2)) => Term::Bool(s1 == s2),
                (Term::Lbl(l1), Term::Lbl(l2)) => Term::Bool(l1 == l2),
                (Term::Sym(s1), Term::Sym(s2)) => Term::Bool(s1 == s2),
                (Term::Record(m1), Term::Record(m2)) => {
                    let (left, center, right) = merge::hashmap::split(m1, m2);

                    if !left.is_empty() || !right.is_empty() {
                        Term::Bool(false)
                    } else {
                        eq_all(
                            center.into_iter().map(|(_, (t1, t2))| (t1, t2)),
                            &env1,
                            &env2,
                            &mut env,
                        )
                    }
                }
                (Term::List(l1), Term::List(l2)) if l1.len() == l2.len() => {
                    eq_all(l1.into_iter().zip(l2.into_iter()), &env1, &env2, &mut env)
                }
                (_, _) => Term::Bool(false),
            };

            Ok(Closure {
                body: res.into(),
                env,
            })
        }
        BinaryOp::LessThan() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Bool(n1 < n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("<, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("<, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::LessOrEq() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Bool(n1 <= n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from("<, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from("<, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::GreaterThan() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Bool(n1 > n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from(">, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from(">, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::GreaterOrEq() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Bool(n1 >= n2).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Num"),
                        op: String::from(">=, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Num"),
                    op: String::from(">=, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::GoField() => {
            if let Term::Str(field) = *t1 {
                if let Term::Lbl(mut l) = *t2 {
                    l.path.push(ty_path::Elem::Field(Ident(field)));
                    Ok(Closure::atomic_closure(Term::Lbl(l).into()))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Label"),
                        op: String::from("goField, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from("goField, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::DynAccess() => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(mut static_map) = *t2 {
                    match static_map.remove(&Ident(id.clone())) {
                        Some(e) => Ok(Closure { body: e, env: env2 }),
                        None => Err(EvalError::FieldMissing {
                            field: id,
                            op: String::from("(.$)"),
                            t: RichTerm {
                                term: Box::new(Term::Record(static_map)),
                                pos: pos2,
                            },
                            pos: pos_op,
                        }),
                    }
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Record"),
                        op: String::from(".$"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from(".$"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::DynExtend(clos) => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(mut static_map) = *t2 {
                    let as_var = clos.body.closurize(&mut env2, clos.env);
                    match static_map.insert(Ident(id.clone()), as_var) {
                        Some(_) => Err(EvalError::Other { msg: format!("$[ .. ]: tried to extend record with the field {}, but it already exists", id), pos: pos_op }),
                        None => Ok(Closure {
                            body: Term::Record(static_map).into(),
                            env: env2,
                        }),
                    }
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Record"),
                        op: String::from("$[ .. ]"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from("$[ .. ]"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::DynRemove() => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(mut static_map) = *t2 {
                    match static_map.remove(&Ident(id.clone())) {
                        None => Err(EvalError::FieldMissing {
                            field: id,
                            op: String::from("(-$)"),
                            t: RichTerm {
                                term: Box::new(Term::Record(static_map)),
                                pos: pos2,
                            },
                            pos: pos_op,
                        }),
                        Some(_) => Ok(Closure {
                            body: Term::Record(static_map).into(),
                            env: env2,
                        }),
                    }
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Record"),
                        op: String::from("-$"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from("-$"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::HasField() => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(static_map) = *t2 {
                    Ok(Closure::atomic_closure(
                        Term::Bool(static_map.contains_key(&Ident(id))).into(),
                    ))
                } else {
                    Err(EvalError::TypeError {
                        expd: String::from("Record"),
                        op: String::from("hasField, 2nd argument"),
                        t: RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                        pos: snd_pos,
                    })
                }
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("Str"),
                    op: String::from("hasField, 1st argument"),
                    t: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    pos: fst_pos,
                })
            }
        }
        BinaryOp::ListConcat() => match (*t1, *t2) {
            (Term::List(ts1), Term::List(ts2)) => {
                let mut env = Environment::new();
                let mut ts: Vec<RichTerm> = ts1
                    .into_iter()
                    .map(|t| t.closurize(&mut env, env1.clone()))
                    .collect();
                ts.extend(ts2.into_iter().map(|t| t.closurize(&mut env, env2.clone())));

                Ok(Closure {
                    body: Term::List(ts).into(),
                    env,
                })
            }
            (Term::List(_), t2) => Err(EvalError::TypeError {
                expd: String::from("List"),
                op: String::from("@, 2nd operand"),
                t: RichTerm {
                    term: Box::new(t2),
                    pos: pos2,
                },
                pos: snd_pos,
            }),
            (t1, _) => Err(EvalError::TypeError {
                expd: String::from("List"),
                op: String::from("@, 1st operand"),
                t: RichTerm {
                    term: Box::new(t1),
                    pos: pos1,
                },
                pos: fst_pos,
            }),
        },
        // This one should not be strict in the first argument (f)
        BinaryOp::ListMap() => {
            if let Term::List(ts) = *t2 {
                let f = RichTerm {
                    term: t1,
                    pos: pos1,
                };
                let f_as_var = f.closurize(&mut env2, env1);

                let ts = ts
                    .into_iter()
                    .map(|t| Term::App(f_as_var.clone(), t).into())
                    .collect();

                Ok(Closure {
                    body: Term::List(ts).into(),
                    env: env2,
                })
            } else {
                Err(EvalError::TypeError {
                    expd: String::from("List"),
                    op: String::from("map, 2nd argument"),
                    t: RichTerm {
                        term: t2,
                        pos: pos2,
                    },
                    pos: snd_pos,
                })
            }
        }
        BinaryOp::ListElemAt() => match (*t1, *t2) {
            (Term::List(mut ts), Term::Num(n)) => {
                let n_int = n as usize;
                if n.fract() != 0.0 {
                    Err(EvalError::Other { msg: format!("elemAt: expected the 2nd agument to be an integer, got the floating-point value {}", n), pos: pos_op })
                } else if n < 0.0 || n_int >= ts.len() {
                    Err(EvalError::Other { msg: format!("elemAt: index out of bounds. Expected a value between 0 and {}, got {}", ts.len(), n), pos: pos_op })
                } else {
                    Ok(Closure {
                        body: ts.swap_remove(n_int),
                        env: env1,
                    })
                }
            }
            (Term::List(_), t2) => Err(EvalError::TypeError {
                expd: String::from("Num"),
                op: String::from("elemAt, 2nd argument"),
                t: RichTerm {
                    term: Box::new(t2),
                    pos: pos2,
                },
                pos: snd_pos,
            }),
            (t1, _) => Err(EvalError::TypeError {
                expd: String::from("List"),
                op: String::from("elemAt, 1st argument"),
                t: RichTerm {
                    term: Box::new(t1),
                    pos: pos1,
                },
                pos: fst_pos,
            }),
        },
        BinaryOp::Merge() => merge(
            RichTerm {
                term: t1,
                pos: pos1,
            },
            env1,
            RichTerm {
                term: t2,
                pos: pos2,
            },
            env2,
            pos_op,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{CallStack, Environment};

    fn some_env() -> Environment {
        HashMap::new()
    }

    #[test]
    fn ite_operation() {
        let cont = OperationCont::Op1(UnaryOp::Ite(), None);
        let mut stack = Stack::new();
        stack.push_arg(Closure::atomic_closure(Term::Num(5.0).into()), None);
        stack.push_arg(Closure::atomic_closure(Term::Num(46.0).into()), None);

        let mut clos = Closure {
            body: Term::Bool(true).into(),
            env: some_env(),
        };

        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();
        let mut strict = true;

        clos = continuate_operation(clos, &mut stack, &mut call_stack, &mut strict).unwrap();

        assert_eq!(
            clos,
            Closure {
                body: Term::Num(46.0).into(),
                env: some_env()
            }
        );
        assert_eq!(0, stack.count_args());
    }

    #[test]
    fn plus_first_term_operation() {
        let cont = OperationCont::Op2First(
            BinaryOp::Plus(),
            Closure {
                body: Term::Num(6.0).into(),
                env: some_env(),
            },
            None,
            true,
        );

        let mut clos = Closure {
            body: Term::Num(7.0).into(),
            env: some_env(),
        };
        let mut stack = Stack::new();
        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();
        let mut strict = true;

        clos = continuate_operation(clos, &mut stack, &mut call_stack, &mut strict).unwrap();

        assert_eq!(
            clos,
            Closure {
                body: Term::Num(6.0).into(),
                env: some_env()
            }
        );

        assert_eq!(1, stack.count_conts());
        assert_eq!(
            (
                OperationCont::Op2Second(
                    BinaryOp::Plus(),
                    Closure {
                        body: Term::Num(7.0).into(),
                        env: some_env(),
                    },
                    None,
                    None,
                    true
                ),
                0,
                None
            ),
            stack.pop_op_cont().expect("Condition already checked.")
        );
    }

    #[test]
    fn plus_second_term_operation() {
        let cont = OperationCont::Op2Second(
            BinaryOp::Plus(),
            Closure {
                body: Term::Num(7.0).into(),
                env: some_env(),
            },
            None,
            None,
            true,
        );
        let mut clos = Closure {
            body: Term::Num(6.0).into(),
            env: some_env(),
        };
        let mut stack = Stack::new();
        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();
        let mut strict = false;

        clos = continuate_operation(clos, &mut stack, &mut call_stack, &mut strict).unwrap();

        assert_eq!(
            clos,
            Closure {
                body: Term::Num(13.0).into(),
                env: some_env()
            }
        );
    }
}
