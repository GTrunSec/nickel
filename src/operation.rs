use eval::{CallStack, Closure, Enviroment, EvalError};
use identifier::Ident;
use label::{solve_label, Label};
use stack::Stack;
use std::cell::RefCell;
use std::rc::Rc;
use term::{BinaryOp, RichTerm, Term, UnaryOp};

#[derive(Debug, PartialEq)]
pub enum OperationCont {
    Op1(UnaryOp, Option<Enviroment>),
    Op2First(BinaryOp, Closure),
    Op2Second(BinaryOp, Closure),
}

pub fn continuate_operation(
    mut clos: Closure,
    stack: &mut Stack,
    call_stack: &mut CallStack,
) -> Result<Closure, EvalError> {
    let (cont, cs_len) = stack.pop_op_cont().expect("Condition already checked");
    call_stack.truncate(cs_len);
    match cont {
        OperationCont::Op1(u_op, env) => process_unary_operation(u_op, env, clos, stack),
        OperationCont::Op2First(b_op, mut snd_clos) => {
            std::mem::swap(&mut clos, &mut snd_clos);
            stack.push_op_cont(OperationCont::Op2Second(b_op, snd_clos), cs_len);
            Ok(clos)
        }
        OperationCont::Op2Second(b_op, fst_clos) => {
            process_binary_operation(b_op, fst_clos, clos, stack)
        }
    }
}

fn process_unary_operation(
    u_op: UnaryOp,
    env_opt: Option<Enviroment>,
    clos: Closure,
    stack: &mut Stack,
) -> Result<Closure, EvalError> {
    let Closure {
        body: RichTerm { term: t, pos: _ },
        env: env,
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
                Err(EvalError::TypeError(format!("Expected Bool, got {:?}", *t)))
            }
        }
        UnaryOp::IsZero() => {
            if let Term::Num(n) = *t {
                // TODO Discuss and decide on this comparison for 0 on f64
                Ok(Closure::atomic_closure(Term::Bool(n == 0.).into()))
            } else {
                Err(EvalError::TypeError(format!("Expected Num, got {:?}", *t)))
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
        UnaryOp::IsFun() => {
            if let Term::Fun(_, _) = *t {
                Ok(Closure::atomic_closure(Term::Bool(true).into()))
            } else {
                Ok(Closure::atomic_closure(Term::Bool(false).into()))
            }
        }
        UnaryOp::Blame(bkp_t) => {
            if let Term::Lbl(l) = *t {
                println!("{:?}", l);
                let res = solve_label(l, true);
                match res {
                    Err(rl) => Err(EvalError::BlameError(rl, None)),
                    Ok(()) => Ok(Closure {
                        env: env_opt.unwrap(),
                        body: *bkp_t,
                    }),
                }
            } else {
                Err(EvalError::TypeError(format!(
                    "Expected Label, got {:?}",
                    *t
                )))
            }
        }
        UnaryOp::SplitFun() => {
            if let Term::Lbl(l) = *t {
                let sb = Rc::new(RefCell::new(false));
                let l1 = Label::Dom(Box::new(l.clone()), sb.clone());
                let l2 = Label::Codom(Box::new(l.clone()), sb.clone());
                // This is a tuple, just squint your eyes
                Ok(Closure::atomic_closure(
                    Term::Fun(
                        Ident("f".into()),
                        RichTerm::app(
                            RichTerm::app(RichTerm::var("f".into()), Term::Lbl(l1).into()),
                            Term::Lbl(l2).into(),
                        ),
                    )
                    .into(),
                ))
            } else {
                Err(EvalError::TypeError(format!(
                    "Expected Label, got {:?}",
                    *t
                )))
            }
        }
        UnaryOp::SplitInter() => {
            if let Term::Lbl(l) = *t {
                let sa = Rc::new(RefCell::new(false));
                let sb = Rc::new(RefCell::new(false));
                let l1 = Label::Inter(Box::new(l.clone()), sa.clone(), sb.clone());
                let l2 = Label::Inter(Box::new(l.clone()), sb.clone(), sa.clone());
                // This is a tuple, just squint your eyes
                Ok(Closure::atomic_closure(
                    Term::Fun(
                        Ident("f".into()),
                        RichTerm::app(
                            RichTerm::app(RichTerm::var("f".into()), Term::Lbl(l1).into()),
                            Term::Lbl(l2).into(),
                        ),
                    )
                    .into(),
                ))
            } else {
                Err(EvalError::TypeError(format!(
                    "Expected Label, got {:?}",
                    *t
                )))
            }
        }
        UnaryOp::SplitUnion() => {
            if let Term::Lbl(l) = *t {
                let sa = Rc::new(RefCell::new(false));
                let sb = Rc::new(RefCell::new(false));
                let l1 = Label::Union(Box::new(l.clone()), sa.clone(), sb.clone());
                let l2 = Label::Union(Box::new(l.clone()), sb.clone(), sa.clone());
                // This is a tuple, just squint your eyes
                Ok(Closure::atomic_closure(
                    Term::Fun(
                        Ident("f".into()),
                        RichTerm::app(
                            RichTerm::app(RichTerm::var("f".into()), Term::Lbl(l1).into()),
                            Term::Lbl(l2).into(),
                        ),
                    )
                    .into(),
                ))
            } else {
                Err(EvalError::TypeError(format!(
                    "Expected Label, got {:?}",
                    *t
                )))
            }
        }
        UnaryOp::DropLbl() => {
            if let Term::Lbl(l) = *t {
                if stack.count_args() >= 1 {
                    let (k, st) = stack.pop_arg().expect("Condition already checked.");

                    let shr = Rc::new(RefCell::new(false));
                    stack.push_unguard(shr.clone());
                    stack.push_arg(
                        Closure::atomic_closure(
                            Term::Lbl(Label::Guard(Box::new(l), shr.clone())).into(),
                        ),
                        st,
                    );

                    println!("fromDrop --> {:?}", k.body);
                    Ok(k)
                } else {
                    panic!("An Drop Lbl wasn't saturated")
                }
            } else {
                Err(EvalError::TypeError(format!(
                    "Expected Label, got {:?}",
                    *t
                )))
            }
        }
    }
}

fn process_binary_operation(
    b_op: BinaryOp,
    fst_clos: Closure,
    clos: Closure,
    _stack: &mut Stack,
) -> Result<Closure, EvalError> {
    let Closure {
        body: RichTerm { term: t1, pos: _ },
        env: _env1,
    } = fst_clos;
    let Closure {
        body: RichTerm { term: t2, pos: _ },
        env: _env2,
    } = clos;
    match b_op {
        BinaryOp::Plus() => {
            if let Term::Num(n1) = *t1 {
                if let Term::Num(n2) = *t2 {
                    Ok(Closure::atomic_closure(Term::Num(n1 + n2).into()))
                } else {
                    Err(EvalError::TypeError(format!("Expected Num, got {:?}", *t2)))
                }
            } else {
                Err(EvalError::TypeError(format!("Expected Num, got {:?}", *t1)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eval::{CallStack, Enviroment};
    use std::collections::HashMap;

    fn some_env() -> Enviroment {
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

        stack.push_op_cont(cont, 0);
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
        stack.push_op_cont(cont, 0);
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
                0
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
        stack.push_op_cont(cont, 0);
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
