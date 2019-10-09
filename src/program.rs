use eval::{eval, CallStack, EvalError, IdentKind, StackElem};
use identifier::Ident;
use label::RLabel;
use parser;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::result::Result;
use term::{RichTerm, Term};

pub struct Program<T: Read> {
    src: T,
    include_contracts: bool,
    read: Option<String>,
    parsed: Option<RichTerm>,
}

impl Program<io::Stdin> {
    pub fn new_from_stdin() -> Program<io::Stdin> {
        Program::new_from_source(io::stdin())
    }
}

impl Program<fs::File> {
    pub fn new_from_file<P: AsRef<Path>>(path: P) -> io::Result<Program<fs::File>> {
        let file = fs::File::open(path)?;
        Ok(Program::new_from_source(file))
    }
}

impl<T: Read> Program<T> {
    pub fn new_from_source(s: T) -> Program<T> {
        Program {
            src: s,
            include_contracts: true,
            read: None,
            parsed: None,
        }
    }

    pub fn eval(&mut self) -> Result<Term, String> {
        let t = self.parse()?;
        match eval(t) {
            Ok(t) => Ok(t),
            Err(EvalError::BlameError(l, cs)) => Err(self.process_blame(l, cs)),
            Err(EvalError::TypeError(s)) => Err(s),
        }
    }

    fn parse(&mut self) -> Result<RichTerm, String> {
        if let None = self.parsed {
            let mut buf = self.read()?;
            if self.include_contracts {
                // TODO get rid of this once we have imports
                buf.insert_str(0, &Self::contracts());
            }
            match parser::grammar::TermParser::new().parse(&buf) {
                Ok(t) => self.parsed = Some(t),
                Err(e) => {
                    return Err(format!("Reached the following error while parsing:\n{}", e));
                }
            };
        }

        if let Some(t) = self.parsed.clone() {
            Ok(t)
        } else {
            Err("Couldn't get the parsed term back.".to_string())
        }
    }

    fn read(&mut self) -> Result<String, String> {
        if let None = self.read {
            let mut buffer = String::new();
            match self.src.read_to_string(&mut buffer) {
                Ok(_) => self.read = Some(buffer),
                Err(e) => {
                    return Err(format!(
                        "Reached the following error while reading the file: '{}'",
                        e
                    ));
                }
            }
        }

        if let Some(b) = self.read.clone() {
            Ok(b)
        } else {
            Err("Couldn't get the file back.".to_string())
        }
    }

    fn process_blame(&mut self, l: RLabel, cs_opt: Option<CallStack>) -> String {
        let mut s = String::new();
        s.push_str("Reached a blame label, some cast went terribly wrong\n");
        s.push_str("    Tag:\n");
        s.push_str(&l.tag);
        s.push_str("\n");

        let pos_from = self.get_line_and_col(l.l);
        let pos_to = self.get_line_and_col(l.r);
        if let (Some((linef, colf)), Some((linet, colt))) = (pos_from, pos_to) {
            if linef == linet {
                s.push_str(&format!(
                    "    Line: {} Columns: {} to {}\n",
                    linef, colf, colt
                ));
            } else {
                s.push_str(&format!("    Line: {} Column: {}\n", linef, colf));
                s.push_str(&format!("    to Line: {} Column: {}\n", linet, colt));
            }
        }

        s.push_str(&format!("    Polarity: {}\n", l.polarity));
        if l.polarity {
            s.push_str("    The blame is on the value (positive blame)\n");
        } else {
            s.push_str("    The blame is on the context (negative blame)\n");
        }

        if let Some(cs) = cs_opt {
            s.push_str("\nCallStack:\n=========\n");
            s = self.show_call_stack(s, cs);
        }
        s
    }

    fn show_call_stack(&mut self, mut s: String, mut cs: CallStack) -> String {
        for e in cs.drain(..).rev() {
            match e {
                StackElem::App(Some((_l, _r))) => {
                    // I'm not sure this App stack is really useful,
                    // will leave it hanging for now
                    //
                    // if let Some((linef, colf)) = self.get_line_and_col(l) {
                    //     s.push_str(&format!(
                    //         "    Applied to a term on line: {} col: {}\n",
                    //         linef, colf
                    //     ));
                    // }
                }
                StackElem::Var(IdentKind::Let(), Ident(x), Some((l, _r))) => {
                    if let Some((linef, colf)) = self.get_line_and_col(l) {
                        s.push_str(&format!(
                            "On a call to {} on line: {} col: {}\n",
                            x, linef, colf
                        ));
                    }
                }
                StackElem::Var(IdentKind::Lam(), Ident(x), Some((l, _r))) => {
                    if let Some((linef, colf)) = self.get_line_and_col(l) {
                        s.push_str(&format!(
                            "    Bound to {} on line: {} col: {}\n",
                            x, linef, colf
                        ));
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn get_line_and_col(&mut self, b: usize) -> Option<(usize, usize)> {
        let buffer = self.read().unwrap();

        let mut line = 1;
        let mut col = 1;
        let mut so_far = Self::contracts().len();
        if b <= so_far {
            // TODO Right now we have some stuff that is just pasted on
            // every file, this check should change and location should be more reliable
            return None;
        }
        for byte in buffer.bytes() {
            so_far = so_far + 1;
            col = col + 1;
            if byte == b'\n' {
                line = line + 1;
                col = 1;
            }
            if so_far == b {
                break;
            }
        }

        Some((line, col))
    }

    fn contracts() -> String {
        "let dyn = fun l => fun t => t in
let num = fun l => fun t => if isNum t then t else blame[ t ] l in
let bool = fun l => fun e => if isBool e then e else blame[ e ] l in
let func = fun s => fun t => fun l => fun e => Unshare(let l1l2 = splitFun l in let l1 = l1l2 (fun x => fun y => x) in let l2 = l1l2 (fun x => fun y => y) in 
if isFun e then (fun x => t l2 (e (s l1 x))) else blame[ e ] l) in

let inter = fun a => fun b => fun l => fun e => Unshare(let l1l2 = splitBranch l in let l1 = l1l2 (fun x => fun y => x) in let l2 = l1l2 (fun x => fun y => y) in
(drop l1) (fun l1d => (drop l2) (fun l2d => a l1d (b l2d e))) ) in
".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn eval_string(s: &str) -> Result<Term, String> {
        let src = Cursor::new(s);

        let mut p = Program::new_from_source(src);
        p.eval()
    }

    #[test]
    fn function_app() {
        let res = eval_string("(fun h => h) 3");

        assert_eq!(Ok(Term::Num(3.0)), res);
    }

    #[test]
    fn let_binding() {
        let res = eval_string("let f = fun f => fun y => f (f y) in f (fun h => h) 3");

        assert_eq!(Ok(Term::Num(3.0)), res);
    }

    #[test]
    fn plus() {
        let res = eval_string("34 + (if true then 2 else 222)");

        assert_eq!(Ok(Term::Num(36.0)), res);
    }

    #[test]
    fn dynamic_if() {
        let res =
            eval_string("let g  = fun x => if x then 0 else false in g ((fun x => true) 23 )");

        assert_eq!(Ok(Term::Num(0.0)), res);
    }

    #[test]
    fn parse_error() {
        let res = eval_string("let g  = funky x => x in g true");

        if let Ok(_) = res {
            panic!("This test should have returned Err()!");
        }
    }

    #[test]
    fn simple_type_check() {
        let res = eval_string("let x = 5 in if isNum x then true else 1");

        assert_eq!(Ok(Term::Bool(true)), res);
    }

    #[test]
    fn fixpoint() {
        let res = eval_string(
            "let Y = (fun f => (fun x => f (x x)) (fun x => f (x x))) in
let g = Y (fun g => (fun x => if x  then (g false) else 4)) in
g true",
        );

        assert_eq!(Ok(Term::Num(4.)), res);
    }

    #[test]
    fn type_contracts() {
        let res = eval_string(
            "let safePlus = Promise(Num -> Num -> Num , fun x => fun y => x + y) in
safePlus Promise(Num , 54) Promise(Num , 6)",
        );

        assert_eq!(Ok(Term::Num(60.)), res);
    }

    #[test]
    fn fibonacci() {
        let res = eval_string(
            "let Y = (fun f => (fun x => f (x x)) (fun x => f (x x))) in
let dec = Promise(Num -> Num, fun x => x + (-1)) in
let or = Promise(Bool -> Bool -> Bool, fun x => fun y => if x then x else y) in

let fibo = Promise(Num -> Num, Y (fun fibo =>
    (fun x => if or (isZero x) (isZero (dec x)) then 1 else (fibo (dec x)) + (fibo (dec (dec x)))))) in
let val = Promise(Num, 4) in
fibo val",
        );

        assert_eq!(Ok(Term::Num(5.)), res);
    }

    #[test]
    fn promise_fail() {
        let res = eval_string(
            "let bool = fun l => fun t => if isBool t then t else blame[t] l in
Promise(Bool, 5)",
        );

        if let Ok(_) = res {
            panic!("This expression should return an error!");
        }
    }

    #[test]
    fn flat_contract_fail() {
        let res = eval_string(
            "let alwaysTrue = fun l => fun t => let boolT = Assume(Bool, t) in 
    if boolT then boolT else blame[boolT] l in
Assume(#alwaysTrue, false)
",
        );
        if let Ok(_) = res {
            panic!("This expression should return an error!");
        }
    }

    #[test]
    fn flat_higher_order_contract() {
        let res = eval_string(
            "let alwaysTrue = fun l => fun t => let boolT = Assume(Bool, t) in 
    if boolT then boolT else blame[boolT] l in
let alwaysFalse = fun l => fun t => let boolT = Assume(Bool, t) in 
    if boolT then  blame[boolT] l else boolT in
let not = fun b => if b then false else true in
Assume(#alwaysTrue -> #alwaysFalse, not ) true
",
        );

        assert_eq!(Ok(Term::Bool(false)), res);
    }

    #[test]
    fn simple_intersections() {
        let res = eval_string("Assume( Num /\\ Num, 34)");
        assert_eq!(Ok(Term::Num(34.)), res);

        let res = eval_string("Assume( Bool /\\ Bool, false)");
        assert_eq!(Ok(Term::Bool(false)), res);

        let res = eval_string("Assume( Num /\\ Bool, true)");
        if let Ok(_) = res {
            panic!("This expression should return an error!");
        }
    }

    #[test]
    fn flat_intersection() {
        let res = eval_string(
            "let alwaysTrue = fun l => fun t => let boolT = Assume(Bool, t) in 
if boolT then boolT else blame[boolT] l in
Assume(Bool /\\ #alwaysTrue, true)
",
        );
        assert_eq!(Ok(Term::Bool(true)), res);

        let res = eval_string(
            "let alwaysTrue = fun l => fun t => let boolT = Assume(Bool, t) in 
if boolT then boolT else blame[boolT] l in
Assume(Bool /\\ #alwaysTrue, false)",
        );
        if let Ok(_) = res {
            panic!("This expression should return an error!");
        }
    }

    #[test]
    fn higher_order_intersection() {
        let res = eval_string(
            "let id = Assume( Bool -> Bool /\\ Num -> Num, (fun x => x)) in
(id 344) + (id 6) 
",
        );
        assert_eq!(Ok(Term::Num(350.)), res);

        let res = eval_string(
            "let id = Assume( (Bool -> Bool) /\\ (Num -> Num), (fun x => x)) in
if id true then 34 else id 344
",
        );
        assert_eq!(Ok(Term::Num(34.)), res);

        // intersection should be per context of elimination
        let res = eval_string(
            "let id = Assume( (Bool -> Bool) /\\ (Num -> Num), (fun x => x)) in
        if id false then 34 else id 344
        ",
        );
        assert_eq!(Ok(Term::Num(344.)), res);
    }

}
