use term::RichTerm;

#[derive(Clone, PartialEq, Debug)]
pub enum Types {
    Dyn(),
    Num(),
    Bool(),
    Arrow(Box<Types>, Box<Types>),
    Inter(Box<Types>, Box<Types>),
    Union(Box<Types>, Box<Types>),
    Flat(RichTerm),
}

impl Types {
    pub fn contract(&self) -> RichTerm {
        match self {
            Types::Dyn() => RichTerm::var("dyn".to_string()),
            Types::Num() => RichTerm::var("num".to_string()),
            Types::Bool() => RichTerm::var("bool".to_string()),
            Types::Arrow(s, t) => RichTerm::app(
                RichTerm::app(RichTerm::var("func".to_string()), s.contract()),
                t.contract(),
            ),
            Types::Inter(s, t) => RichTerm::app(
                RichTerm::app(RichTerm::var("inter".to_string()), s.contract()),
                t.contract(),
            ),
            Types::Union(s, t) => RichTerm::app(
                RichTerm::app(RichTerm::var("union".to_string()), s.contract()),
                t.contract(),
            ),
            Types::Flat(t) => t.clone(),
        }
    }
}
