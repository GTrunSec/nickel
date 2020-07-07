use crate::eval::Environment;
use crate::eval::{CallStack, Closure, EvalError, IdentKind};
use crate::identifier::Ident;
use crate::label::TyPath;
use crate::merge::merge;
use crate::position::Span;
use crate::stack::Stack;
use crate::term::{BinaryOp, RichTerm, Term, UnaryOp};
use simple_counter::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

generate_counter!(FreshVariableCounter, usize);

#[derive(Debug, PartialEq)]
pub enum OperationCont {
    Op1(UnaryOp<Closure>),
    Op2First(BinaryOp<Closure>, Closure),
    Op2Second(BinaryOp<Closure>, Closure),
}

pub fn continuate_operation(
    mut clos: Closure,
    stack: &mut Stack,
    call_stack: &mut CallStack,
) -> Result<Closure, EvalError> {
    let (cont, cs_len, pos) = stack.pop_op_cont().expect("Condition already checked");
    call_stack.truncate(cs_len);
    match cont {
        OperationCont::Op1(u_op) => process_unary_operation(u_op, clos, stack, pos),
        OperationCont::Op2First(b_op, mut snd_clos) => {
            std::mem::swap(&mut clos, &mut snd_clos);
            stack.push_op_cont(OperationCont::Op2Second(b_op, snd_clos), cs_len, pos);
            Ok(clos)
        }
        OperationCont::Op2Second(b_op, fst_clos) => {
            process_binary_operation(b_op, fst_clos, clos, stack, pos)
        }
    }
}

fn process_unary_operation(
    u_op: UnaryOp<Closure>,
    clos: Closure,
    stack: &mut Stack,
    pos_op: Option<Span>,
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
                Err(EvalError::TypeError(
                    String::from("Bool"),
                    String::from("if"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::IsZero() => {
            if let Term::Num(n) = *t {
                // TODO Discuss and decide on this comparison for 0 on f64
                Ok(Closure::atomic_closure(Term::Bool(n == 0.).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Num"),
                    String::from("isZero"),
                    RichTerm { term: t, pos },
                ))
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
        UnaryOp::Blame() => {
            if let Term::Lbl(l) = *t {
                Err(EvalError::BlameError(l, None))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("blame"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Embed(_id) => {
            if let en @ Term::Enum(_) = *t {
                Ok(Closure::atomic_closure(en.into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Enum"),
                    String::from("embed"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Switch(mut m, d) => {
            if let Term::Enum(en) = *t {
                match m.remove(&en) {
                    Some(clos) => Ok(clos),
                    None => match d {
                        Some(clos) => Ok(clos),
                        None => Err(EvalError::TypeError(
                            String::from("Enum"),
                            String::from("switch"),
                            RichTerm {
                                term: Box::new(Term::Enum(en)),
                                pos,
                            },
                        )),
                    },
                }
            } else {
                match d {
                    Some(clos) => Ok(clos),
                    None => Err(EvalError::TypeError(
                        String::from("Enum"),
                        String::from("switch"),
                        RichTerm { term: t, pos },
                    )),
                }
            }
        }
        UnaryOp::ChangePolarity() => {
            if let Term::Lbl(mut l) = *t {
                l.polarity = !l.polarity;
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("changePolarity"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Pol() => {
            if let Term::Lbl(l) = *t {
                Ok(Closure::atomic_closure(Term::Bool(l.polarity).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("polarity"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::GoDom() => {
            if let Term::Lbl(mut l) = *t {
                l.path = TyPath::Domain(Box::new(l.path.clone()));
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("goDom"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::GoCodom() => {
            if let Term::Lbl(mut l) = *t {
                l.path = TyPath::Codomain(Box::new(l.path.clone()));
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("goCodom"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Tag(s) => {
            if let Term::Lbl(mut l) = *t {
                l.tag.push_str("\n");
                l.tag.push_str(&s);
                Ok(Closure::atomic_closure(Term::Lbl(l).into()))
            } else {
                Err(EvalError::TypeError(
                    String::from("Label"),
                    String::from("tag"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Wrap() => {
            if let Term::Sym(s) = *t {
                Ok(Closure::atomic_closure(
                    Term::Fun(
                        Ident("x".to_string()),
                        Term::Wrapped(s, RichTerm::var("x".to_string())).into(),
                    )
                    .into(),
                ))
            } else {
                Err(EvalError::TypeError(
                    String::from("Sym"),
                    String::from("wrap"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::StaticAccess(id) => {
            if let Term::Record(mut static_map) = *t {
                match static_map.remove(&id) {
                    Some(e) => Ok(Closure { body: e, env }),

                    None => Err(EvalError::FieldMissing(
                        id.0,
                        String::from("(.)"),
                        RichTerm {
                            term: Box::new(Term::Record(static_map)),
                            pos,
                        },
                        pos_op,
                    )), //TODO include the position of operators on the stack
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Record"),
                    String::from("field access"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::MapRec(f) => {
            if let Term::Record(rec) = *t {
                let fresh_var = format!("_{}", env.len());

                env.insert(
                    Ident(fresh_var.clone()),
                    (Rc::new(RefCell::new(f)), IdentKind::Record()),
                );

                let rec = rec
                    .into_iter()
                    .map(|e| {
                        let (Ident(s), t) = e;
                        (
                            Ident(s.clone()),
                            Term::App(
                                Term::App(
                                    Term::Var(Ident(fresh_var.clone())).into(),
                                    Term::Str(s.clone()).into(),
                                )
                                .into(),
                                t.clone(),
                            )
                            .into(),
                        )
                    })
                    .collect();

                Ok(Closure {
                    body: Term::Record(rec).into(),
                    env,
                })
            } else {
                Err(EvalError::TypeError(
                    String::from("Record"),
                    String::from("map on record"),
                    RichTerm { term: t, pos },
                ))
            }
        }
        UnaryOp::Seq() => {
            if stack.count_args() >= 1 {
                let (next, _) = stack.pop_arg().expect("Condition already checked.");
                Ok(next)
            } else {
                Err(EvalError::NotEnoughArgs(2, String::from("seq"), pos_op))
            }
        }
        UnaryOp::DeepSeq() => {
            /// Build a closure that forces a given list of terms, and at the end resumes the
            /// evaluation of the argument on the top of the stack.
            /// Requires its first argument to be non-empty
            fn seq_terms<I>(mut terms: I, env: Environment) -> Result<Closure, EvalError>
            where
                I: Iterator<Item = RichTerm>,
            {
                let first = terms
                    .next()
                    .expect("expected the argument to be a non-empty iterator");
                let body = terms.fold(Term::Op1(UnaryOp::DeepSeq(), first).into(), |acc, t| {
                    Term::App(Term::Op1(UnaryOp::DeepSeq(), t).into(), acc).into()
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
                        Err(EvalError::NotEnoughArgs(2, String::from("deepSeq"), pos_op))
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
                    Err(EvalError::Other(String::from("head: empty list"), pos_op))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("List"),
                    String::from("head"),
                    RichTerm { term: t, pos },
                ))
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
                    Err(EvalError::Other(String::from("tail: empty list"), pos_op))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("List"),
                    String::from("tail"),
                    RichTerm { term: t, pos },
                ))
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
                Err(EvalError::TypeError(
                    String::from("List"),
                    String::from("length"),
                    RichTerm { term: t, pos },
                ))
            }
        }
    }
}

fn process_binary_operation(
    b_op: BinaryOp<Closure>,
    fst_clos: Closure,
    clos: Closure,
    _stack: &mut Stack,
    pos_op: Option<Span>,
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
                    Err(EvalError::TypeError(
                        String::from("Num"),
                        String::from("+, 2nd argument"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Num"),
                    String::from("+, 1st argument"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::PlusStr() => {
            if let Term::Str(s1) = *t1 {
                if let Term::Str(s2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Str(s1 + &s2).into()))
                } else {
                    Err(EvalError::TypeError(
                        String::from("Str"),
                        String::from("++, 1st argument"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Str"),
                    String::from("++, 2nd argument"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::Unwrap() => {
            if let Term::Sym(s1) = *t1 {
                // Return a function that either behaves like the identity or
                // const unwrapped_term

                Ok(if let Term::Wrapped(s2, t) = *t2 {
                    if s1 == s2 {
                        Closure {
                            body: Term::Fun(Ident("-invld".to_string()), t).into(),
                            env: env2,
                        }
                    } else {
                        Closure::atomic_closure(
                            Term::Fun(Ident("x".to_string()), RichTerm::var("x".to_string()))
                                .into(),
                        )
                    }
                } else {
                    Closure::atomic_closure(
                        Term::Fun(Ident("x".to_string()), RichTerm::var("x".to_string())).into(),
                    )
                })
            } else {
                Err(EvalError::TypeError(
                    String::from("Sym"),
                    String::from("unwrap, 1st argument"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::EqBool() => {
            if let Term::Bool(b1) = *t1 {
                if let Term::Bool(b2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Bool(b1 == b2).into()))
                } else {
                    Err(EvalError::TypeError(
                        String::from("Bool"),
                        String::from("== [bool], 1st argument"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Bool"),
                    String::from("== [bool], 2nd argument"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::DynAccess() => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(mut static_map) = *t2 {
                    match static_map.remove(&Ident(id.clone())) {
                        Some(e) => Ok(Closure { body: e, env: env2 }),
                        None => Err(EvalError::FieldMissing(
                            format!("{}", id),
                            String::from("(.$)"),
                            RichTerm {
                                term: Box::new(Term::Record(static_map)),
                                pos: pos2,
                            },
                            pos_op,
                        )),
                    }
                } else {
                    Err(EvalError::TypeError(
                        String::from("Record"),
                        String::from(".$"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Str"),
                    String::from(".$"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::DynExtend(clos) => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(mut static_map) = *t2 {
                    // Arnauds trick, make the closure into a fresh variable
                    // TODO: refactor via closurize() once the PR on merge has landed
                    let fresh_var = format!("_{}", FreshVariableCounter::next());

                    env2.insert(
                        Ident(fresh_var.clone()),
                        (Rc::new(RefCell::new(clos)), IdentKind::Record()),
                    );

                    match static_map.insert(Ident(id.clone()), Term::Var(Ident(fresh_var)).into()) {
                        Some(_) =>
                        //TODO Upgrade, uninformative error
                        {
                            Err(EvalError::Other(format!("$[ .. ]: tried to extend record with the field {}, but it already exists", id), pos_op))
                        }
                        None => Ok(Closure {
                            body: Term::Record(static_map).into(),
                            env: env2,
                        }),
                    }
                } else {
                    Err(EvalError::TypeError(
                        String::from("Record"),
                        String::from("$[ .. ]"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Str"),
                    String::from("$[ .. ]"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::DynRemove() => {
            if let Term::Record(mut static_map) = *t1 {
                if let Term::Str(id) = *t2 {
                    match static_map.remove(&Ident(id.clone())) {
                        None => Err(EvalError::FieldMissing(
                            format!("{}", id),
                            String::from("(-$)"),
                            RichTerm {
                                term: Box::new(Term::Record(static_map)),
                                pos: pos1,
                            },
                            pos_op,
                        )),
                        Some(_) => Ok(Closure {
                            body: Term::Record(static_map).into(),
                            env: env1,
                        }),
                    }
                } else {
                    Err(EvalError::TypeError(
                        String::from("Str"),
                        String::from("-$"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Record"),
                    String::from("-$"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::HasField() => {
            if let Term::Str(id) = *t1 {
                if let Term::Record(static_map) = *t2 {
                    Ok(Closure::atomic_closure(
                        Term::Bool(static_map.contains_key(&Ident(id))).into(),
                    ))
                } else {
                    Err(EvalError::TypeError(
                        String::from("Record"),
                        String::from("hasField, 2nd argument"),
                        RichTerm {
                            term: t2,
                            pos: pos2,
                        },
                    ))
                }
            } else {
                Err(EvalError::TypeError(
                    String::from("Str"),
                    String::from("hasField, 1st argument"),
                    RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                ))
            }
        }
        BinaryOp::ListConcat() => match (*t1, *t2) {
            (Term::List(mut ts1), Term::List(ts2)) => {
                ts1.extend(ts2);
                Ok(Closure::atomic_closure(Term::List(ts1).into()))
            }
            (Term::List(_), t2) => Err(EvalError::TypeError(
                String::from("List"),
                String::from("@ 2nd operand"),
                RichTerm {
                    term: Box::new(t2),
                    pos: pos2,
                },
            )),
            (t1, _) => Err(EvalError::TypeError(
                String::from("List"),
                String::from("@, first operand"),
                RichTerm {
                    term: Box::new(t1),
                    pos: pos1,
                },
            )),
        },
        // This one should not be strict in the first argument (f)
        BinaryOp::ListMap() => {
            if let Term::List(mut ts) = *t2 {
                // TODO: refactor via closurize() once the PR on merge has landed
                let fresh_var = format!("_{}", FreshVariableCounter::next());

                let f_closure = Closure {
                    body: RichTerm {
                        term: t1,
                        pos: pos1,
                    },
                    env: env1,
                };
                let f_as_var: RichTerm = Term::Var(Ident(fresh_var.clone())).into();

                env2.insert(
                    Ident(fresh_var.clone()),
                    (Rc::new(RefCell::new(f_closure)), IdentKind::Record()),
                );

                let ts = ts
                    .into_iter()
                    .map(|t| Term::App(f_as_var.clone(), t).into())
                    .collect();

                Ok(Closure {
                    body: Term::List(ts).into(),
                    env: env2,
                })
            } else {
                Err(EvalError::TypeError(
                    String::from("List"),
                    String::from("map, 2nd argument"),
                    RichTerm {
                        term: t2,
                        pos: pos2,
                    },
                ))
            }
        }
        BinaryOp::ListElemAt() => match (*t1, *t2) {
            (Term::List(mut ts), Term::Num(n)) => {
                let n_int = n as usize;
                if n.fract() != 0.0 {
                    Err(EvalError::Other(format!("elemAt: expected the second agument to be an integer, got the floating-point value {}", n), pos_op))
                } else if n_int >= ts.len() {
                    Err(EvalError::Other(format!("elemAt: index out of bounds. Expected a value between 0 and {}, got {})", ts.len(), n_int), pos_op))
                } else {
                    Ok(Closure {
                        body: ts.swap_remove(n_int),
                        env: env1,
                    })
                }
            }
            (Term::List(_), t2) => Err(EvalError::TypeError(
                String::from("Num"),
                String::from("elemAt, 2nd argument"),
                RichTerm {
                    term: Box::new(t2),
                    pos: pos2,
                },
            )),
            (t1, _) => Err(EvalError::TypeError(
                String::from("List"),
                String::from("elemAt, 1st argument"),
                RichTerm {
                    term: Box::new(t1),
                    pos: pos1,
                },
            )),
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
        let cont = OperationCont::Op1(UnaryOp::Ite());
        let mut stack = Stack::new();
        stack.push_arg(Closure::atomic_closure(Term::Num(5.0).into()), None);
        stack.push_arg(Closure::atomic_closure(Term::Num(46.0).into()), None);

        let mut clos = Closure {
            body: Term::Bool(true).into(),
            env: some_env(),
        };

        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();

        clos = continuate_operation(clos, &mut stack, &mut call_stack).unwrap();

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
        );

        let mut clos = Closure {
            body: Term::Num(7.0).into(),
            env: some_env(),
        };
        let mut stack = Stack::new();
        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();

        clos = continuate_operation(clos, &mut stack, &mut call_stack).unwrap();

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
                    }
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
        );
        let mut clos = Closure {
            body: Term::Num(6.0).into(),
            env: some_env(),
        };
        let mut stack = Stack::new();
        stack.push_op_cont(cont, 0, None);
        let mut call_stack = CallStack::new();

        clos = continuate_operation(clos, &mut stack, &mut call_stack).unwrap();

        assert_eq!(
            clos,
            Closure {
                body: Term::Num(13.0).into(),
                env: some_env()
            }
        );
    }
}
