/// Closure conversion for HIR
/// We translate into HIR/CC, then lambda lift, producing HIR again (but without lambdas).

use rpds::HashTrieSet;
use std::collections::HashMap;
use super::trees::{Stm, Exp, Type, Def, Param, Field, Lit, Root};
use crate::common::names::*;
use crate::hir::ops::*;

// Closure converted expressions and statements.
// This is just a duplicate of Exp, but Lambda and Apply are different.
// The purpose of this is to ensure all the tree is rewritten. We transform from
// HIR to HIR/CC, then back to HIR (without Lambda).
mod hircc {
    use crate::hir::trees::Type;
    use crate::hir::trees::Param;
    use crate::hir::trees::Lit;
    use crate::common::names::Name;
    use crate::hir::ops::*;

    #[derive(Clone, Debug)]
    pub enum Exp {
        NewArray { ty: Type, length: Box<Exp> },
        ArrayLit { ty: Type, exps: Vec<Exp> },
        ArrayLoad { bounds_check: bool, ty: Type, array: Box<Exp>, index: Box<Exp> },
        ArrayLength { array: Box<Exp> },

        Lit { lit: Lit },
        Call { fun_type: Type, name: Name, args: Vec<Exp> },
        Var { name: Name, ty: Type },

        Binary { op: Bop, e1: Box<Exp>, e2: Box<Exp> },
        Unary { op: Uop, exp: Box<Exp> },

        Seq { body: Box<Stm>, exp: Box<Exp> },

        Let { inits: Vec<Field>, body: Box<Exp> },
        LambdaCC { ret_type: Type, env_param: Param, params: Vec<Param>, body: Box<Exp> },
        ApplyCC { fun_type: Type, fun: Box<Exp>, args: Vec<Exp> },

        StructLit { fields: Vec<Field> },
        StructLoad { ty: Type, base: Box<Exp>, field: Name },

        Box { ty: Type, exp: Box<Exp> },
        Unbox { ty: Type, exp: Box<Exp> },
        Cast { ty: Type, exp: Box<Exp> },
    }

    #[derive(Clone, Debug)]
    pub enum Stm {
        IfElse { cond: Box<Exp>, if_true: Box<Stm>, if_false: Box<Stm> },
        IfThen { cond: Box<Exp>, if_true: Box<Stm> },
        While { cond: Box<Exp>, body: Box<Stm> },
        Return { exp: Box<Exp> },
        Block { body: Vec<Stm> },
        Eval { exp: Box<Exp> },
        Assign { ty: Type, lhs: Name, rhs: Box<Exp> },
        ArrayAssign { bounds_check: bool, ty: Type, array: Box<Exp>, index: Box<Exp>, value: Box<Exp> },
        StructAssign { ty: Type, base: Box<Exp>, field: Name, value: Box<Exp> },
    }

    #[derive(Clone, Debug)]
    pub struct Field {
        pub param: Param,
        pub exp: Box<Exp>,
    }
}

macro_rules! union {
    ($e: expr) => { $e };

    ($e1: expr, $e2: expr) => {
        {
            let mut s1: HashTrieSet<Name> = $e1;
            let s2: HashTrieSet<Name> = $e2;
            for x in s2.iter() {
                s1 = s1.insert(*x);
            }
            s1
        }
    };

    ($e: expr, $($es: expr),+) => {
        union!($e, union!($($es),+))
    };
}

trait FV {
    fn fv(&self) -> HashTrieSet<Name>;
}

impl<A: FV> FV for Vec<A> {
    fn fv(&self) -> HashTrieSet<Name> {
        let mut s = HashTrieSet::new();
        for e in self {
            s = union!(s, e.fv());
        }
        s
    }
}

impl FV for Stm {
    fn fv(&self) -> HashTrieSet<Name> {
        match self {
            Stm::IfElse { cond, if_true, if_false } => {
                union!(cond.fv(), if_true.fv(), if_false.fv())
            },
            Stm::IfThen { cond, if_true } => {
                union!(cond.fv(), if_true.fv())
            },
            Stm::While { cond, body } => {
                union!(cond.fv(), body.fv())
            },
            Stm::Return { exp } => {
                exp.fv()
            },
            Stm::Block { body } => {
                body.fv()
            },
            Stm::Eval { exp } => {
                exp.fv()
            },
            Stm::Assign { ty, lhs, rhs } => {
                rhs.fv().insert(*lhs)
            },
            Stm::ArrayAssign { bounds_check, ty, array, index, value } => {
                union!(array.fv(), index.fv(), value.fv())
            },
            Stm::StructAssign { ty, base, field, value } => {
                union!(base.fv(), value.fv())
            },
        }
    }
}

impl FV for Exp {
    fn fv(&self) -> HashTrieSet<Name> {
        match self {
            Exp::NewArray { ty, length } => length.fv(),
            Exp::ArrayLit { ty, exps } => {
                exps.fv()
            },
            Exp::ArrayLoad { bounds_check, ty, array, index } => {
                union!(array.fv(), index.fv())
            },
            Exp::ArrayLength { array } => array.fv(),

            Exp::Lit { lit } => {
                HashTrieSet::new()
            }
            Exp::Call { fun_type, name, args } => {
                args.fv()
            },
            Exp::Var { name, ty } => {
                HashTrieSet::new().insert(*name)
            },
            Exp::Binary { op, e1, e2 } => {
                union!(e1.fv(), e2.fv())
            },
            Exp::Unary { op, exp } => exp.fv(),
            Exp::Box { ty, exp } => exp.fv(),
            Exp::Unbox { ty, exp } => exp.fv(),
            Exp::Cast { ty, exp } => exp.fv(),

            Exp::Seq { body, exp } => {
                union!(body.fv(), exp.fv())
            },
            Exp::Let { inits, body } => {
                let mut p = HashTrieSet::new();
                for init in inits {
                    p = p.insert(init.param.name);
                }
                let mut s = HashTrieSet::new();
                for x in body.fv().iter() {
                    if ! p.contains(&x) {
                        s = s.insert(*x);
                    }
                }
                for init in inits {
                    s = union!(s, init.exp.fv());
                }
                s
            },
            Exp::Lambda { ret_type, params, body } => {
                let mut p = HashTrieSet::new();
                for param in params {
                    p = p.insert(param.name);
                }
                let mut s = HashTrieSet::new();
                for x in body.fv().iter() {
                    if ! p.contains(&x) {
                        s = s.insert(*x);
                    }
                }
                s
            },
            Exp::Apply { fun_type, fun, args } => {
                union!(fun.fv(), args.fv())
            }
            Exp::StructLit { fields } => {
                let mut s = HashTrieSet::new();
                for field in fields {
                    s = union!(s, field.exp.fv())
                }
                s
            },
            Exp::StructLoad { ty, base, field } => base.fv(),
        }
    }
}

type Subst = HashMap<Name, hircc::Exp>;

trait Substitute {
    fn subst(&self, s: &Subst) -> Self;
}

impl<A: Substitute + Clone> Substitute for Box<A> {
    fn subst(&self, s: &Subst) -> Box<A> {
        Box::new((*self.clone()).subst(s))
    }
}

impl<A: Substitute> Substitute for Vec<A> {
    fn subst(&self, s: &Subst) -> Vec<A> {
        self.iter().map(|e| e.subst(s)).collect()
    }
}

impl Substitute for hircc::Field {
    fn subst(&self, s: &Subst) -> hircc::Field {
        hircc::Field {
            param: self.param.clone(),
            exp: self.exp.subst(s)
        }
    }
}

impl Substitute for hircc::Exp {
    fn subst(&self, s: &Subst) -> hircc::Exp {
        match self {
            hircc::Exp::NewArray { ty, length } => {
                hircc::Exp::NewArray { ty: ty.clone(), length: length.subst(s) }
            },
            hircc::Exp::ArrayLit { ty, exps } => {
                hircc::Exp::ArrayLit { ty: ty.clone(), exps: exps.subst(s) }
            },
            hircc::Exp::ArrayLoad { bounds_check, ty, array, index } => {
                hircc::Exp::ArrayLoad { bounds_check: *bounds_check, ty: ty.clone(), array: array.subst(s), index: index.subst(s) }
            },
            hircc::Exp::ArrayLength { array } => {
                hircc::Exp::ArrayLength { array: array.subst(s) }
            },
            hircc::Exp::Lit { lit } => {
                hircc::Exp::Lit { lit: lit.clone() }
            },
            hircc::Exp::Call { fun_type, name, args } => {
                hircc::Exp::Call { fun_type: fun_type.clone(), name: *name, args: args.subst(s) }
            },
            hircc::Exp::Var { name, ty } => {
                match s.get(&name) {
                    Some(e) => e.clone(),
                    None => hircc::Exp::Var { name: *name, ty: ty.clone() }
                }
            },
            hircc::Exp::Binary { op, e1, e2 } => {
                hircc::Exp::Binary { op: *op, e1: e1.subst(s), e2: e2.subst(s) }
            },
            hircc::Exp::Unary { op, exp } => {
                hircc::Exp::Unary { op: *op, exp: exp.subst(s) }
            },
            hircc::Exp::Box { ty, exp } => {
                hircc::Exp::Box { ty: ty.clone(), exp: exp.subst(s) }
            },
            hircc::Exp::Unbox { ty, exp } => {
                hircc::Exp::Unbox { ty: ty.clone(), exp: exp.subst(s) }
            },
            hircc::Exp::Cast { ty, exp } => {
                hircc::Exp::Cast { ty: ty.clone(), exp: exp.subst(s) }
            },

            hircc::Exp::Seq { body, exp } => {
                hircc::Exp::Seq { body: body.subst(s), exp: exp.subst(s) }
            },

            hircc::Exp::Let { inits, body } => {
                let mut s2: Subst = s.clone();
                for f in inits {
                    let name = f.param.name;
                    s2.remove(&name);
                }
                hircc::Exp::Let { inits: inits.subst(s), body: body.subst(&s2) }
            },
            hircc::Exp::LambdaCC { ret_type, env_param, params, body } => {
                let mut s2: Subst = s.clone();
                s2.remove(&env_param.name);
                for param in params {
                    s2.remove(&param.name);
                }
                hircc::Exp::LambdaCC { ret_type: ret_type.clone(), env_param: env_param.clone(), params: params.clone(), body: body.subst(&s2) }
            },
            hircc::Exp::ApplyCC { fun_type, fun, args } => {
                hircc::Exp::ApplyCC { fun_type: fun_type.clone(), fun: fun.subst(s), args: args.subst(s) }
            },

            hircc::Exp::StructLit { fields } => {
                hircc::Exp::StructLit { fields: fields.subst(s) }
            },
            hircc::Exp::StructLoad { ty, base, field } => {
                hircc::Exp::StructLoad { ty: ty.clone(), base: base.subst(s), field: *field }
            },
        }
    }
}

impl Substitute for hircc::Stm {
    fn subst(&self, s: &Subst) -> hircc::Stm {
        match self {
            hircc::Stm::IfElse { cond, if_true, if_false } => {
                hircc::Stm::IfElse { cond: cond.subst(s), if_true: if_true.subst(s), if_false: if_false.subst(s) }
            },
            hircc::Stm::IfThen { cond, if_true } => {
                hircc::Stm::IfThen { cond: cond.subst(s), if_true: if_true.subst(s) }
            },
            hircc::Stm::While { cond, body } => {
                hircc::Stm::While { cond: cond.subst(s), body: body.subst(s) }
            },
            hircc::Stm::Return { exp } => {
                hircc::Stm::Return { exp: exp.subst(s) }
            },
            hircc::Stm::Block { body } => {
                hircc::Stm::Block { body: body.subst(s) }
            },
            hircc::Stm::Eval { exp } => {
                hircc::Stm::Eval { exp: exp.subst(s) }
            },
            hircc::Stm::Assign { ty, lhs, rhs } => {
                hircc::Stm::Assign { ty: ty.clone(), lhs: *lhs, rhs: rhs.subst(s) }
            },
            hircc::Stm::ArrayAssign { bounds_check, ty, array, index, value } => {
                hircc::Stm::ArrayAssign { bounds_check: *bounds_check, ty: ty.clone(), array: array.subst(s), index: index.subst(s), value: value.subst(s) }
            },
            hircc::Stm::StructAssign { ty, base, field, value } => {
                hircc::Stm::StructAssign { ty: ty.clone(), base: base.subst(s), field: *field, value: value.subst(s) }
            },
        }
    }
}

pub trait CC<T> {
    fn convert(&self) -> T;
}

impl CC<hircc::Field> for Field {
    fn convert(&self) -> hircc::Field {
        hircc::Field {
            param: self.param.clone(),
            exp: Box::new(self.exp.convert())
        }
    }
}

impl CC<hircc::Exp> for Exp {
    fn convert(&self) -> hircc::Exp {
        match self {
            Exp::NewArray { ty, length } => {
                hircc::Exp::NewArray { ty: ty.clone(), length: Box::new(length.convert()) }
            },
            Exp::ArrayLit { ty, exps } => {
                hircc::Exp::ArrayLit { ty: ty.clone(), exps: exps.iter().map(|e| e.convert()).collect() }
            },
            Exp::ArrayLoad { bounds_check, ty, array, index } => {
                hircc::Exp::ArrayLoad { bounds_check: *bounds_check, ty: ty.clone(), array: Box::new(array.convert()), index: Box::new(index.convert()) }
            },
            Exp::ArrayLength { array } => {
                hircc::Exp::ArrayLength { array: Box::new(array.convert()) }
            },
            Exp::Lit { lit } => {
                hircc::Exp::Lit { lit: lit.clone() }
            },
            Exp::Call { fun_type, name, args } => {
                hircc::Exp::Call { fun_type: fun_type.clone(), name: *name, args: args.iter().map(|e| e.convert()).collect() }
            },
            Exp::Var { name, ty } => {
                hircc::Exp::Var { name: *name, ty: ty.clone() }
            },

            Exp::Binary { op, e1, e2 } => {
                hircc::Exp::Binary { op: *op, e1: Box::new(e1.convert()), e2: Box::new(e2.convert()) }
            },
            Exp::Unary { op, exp } => {
                hircc::Exp::Unary { op: *op, exp: Box::new(exp.convert()) }
            },
            Exp::Box { ty, exp } => {
                hircc::Exp::Box { ty: ty.clone(), exp: Box::new(exp.convert()) }
            },
            Exp::Unbox { ty, exp } => {
                hircc::Exp::Unbox { ty: ty.clone(), exp: Box::new(exp.convert()) }
            },
            Exp::Cast { ty, exp } => {
                hircc::Exp::Cast { ty: ty.clone(), exp: Box::new(exp.convert()) }
            },

            Exp::Seq { body, exp } => {
                hircc::Exp::Seq { body: Box::new(body.convert()), exp: Box::new(exp.convert()) }
            },

            Exp::Let { inits, body } => {
                hircc::Exp::Let { inits: inits.iter().map(|f| f.convert()).collect(), body: Box::new(body.convert()) }
            },
            Exp::Lambda { ret_type, params, body } => {
                // The only interesting case is lambda.

                // Create a new name for the environment parameter.
                let env = Name::fresh("env");

                // Get the free variables of the lambda.
                // TODO: get the types of the variables!
                let vars = self.fv();

                // Create a struct to represent the environment.
                // Each var in vars is mapped to a lookup into the environment.
                let mut env_fields = Vec::new();
                let mut env_params = Vec::new();

                for (i, x) in vars.iter().enumerate() {
                    // Make sure the indices agree.
                    assert_eq!(env_fields.len(), i);
                    let param = Param {
                        ty: Type::Box,
                        name: *x
                    };
                    env_params.push(param.clone());
                    env_fields.push(hircc::Field {
                        param: param,
                        exp: Box::new(hircc::Exp::Var { name: *x, ty: Type::Box }),
                    });
                }

                let internal_env_type = Type::Struct { fields: env_params };
                let external_env_type = Type::Struct { fields: vec![] };   // the environment type as seen by the caller

                let mut arg_types = Vec::new();
                arg_types.extend(params.iter().map(|p| p.ty.clone()));
                arg_types.push(external_env_type.clone());

                let fun_type = Type::Fun {
                    ret: Box::new(ret_type.clone()),
                    args: arg_types,
                };

                // Build a substitution.
                // Map x to env.x
                let mut s = HashMap::new();
                for (i, x) in vars.iter().enumerate() {
                    s.insert(*x, hircc::Exp::StructLoad {
                        ty: internal_env_type.clone(),
                        base: Box::new(hircc::Exp::Var { name: env, ty: internal_env_type.clone() }),
                        field: *x
                    });
                }

                let cc_body = body.convert().subst(&s);

                let fun_field = Param { name: Name::new("fun"), ty: fun_type.clone() };
                let env_field = Param { name: Name::new("env"), ty: external_env_type.clone() };

                hircc::Exp::StructLit {
                    fields: vec![
                        hircc::Field {
                            param: fun_field,
                            exp: Box::new(
                                hircc::Exp::LambdaCC {
                                    ret_type: ret_type.clone(),
                                    env_param: Param {
                                        name: env,
                                        ty: internal_env_type.clone(),
                                    },
                                    params: params.clone(),
                                    body: Box::new(cc_body),
                                }
                            ),
                        },
                        hircc::Field {
                            param: env_field,
                            exp: Box::new(
                                hircc::Exp::Cast {
                                    ty: external_env_type.clone(),
                                    exp: Box::new(
                                        hircc::Exp::StructLit {
                                            fields: env_fields
                                        }
                                    )
                                }
                            ),
                        }
                    ]
                }
            },
            Exp::Apply { fun_type, fun, args } => {
                hircc::Exp::ApplyCC { fun_type: fun_type.clone(), fun: Box::new(fun.convert()), args: args.iter().map(|e| e.convert()).collect() }
            },

            Exp::StructLit { fields } => {
                hircc::Exp::StructLit {
                    fields: fields.iter().map(|f| hircc::Field { param: f.param.clone(), exp: Box::new(f.exp.convert()) }).collect()
                }
            },
            Exp::StructLoad { ty, base, field } => {
                hircc::Exp::StructLoad { ty: ty.clone(), base: Box::new(base.convert()), field: *field }
            },
        }
    }
}

impl CC<hircc::Stm> for Stm {
    fn convert(&self) -> hircc::Stm {
        match self {
            Stm::IfElse { cond, if_true, if_false } => {
                hircc::Stm::IfElse { cond: Box::new(cond.convert()), if_true: Box::new(if_true.convert()), if_false: Box::new(if_false.convert()) }
            },
            Stm::IfThen { cond, if_true } => {
                hircc::Stm::IfThen { cond: Box::new(cond.convert()), if_true: Box::new(if_true.convert()) }
            },
            Stm::While { cond, body } => {
                hircc::Stm::While { cond: Box::new(cond.convert()), body: Box::new(body.convert()) }
            },
            Stm::Return { exp } => {
                hircc::Stm::Return { exp: Box::new(exp.convert()) }
            },
            Stm::Block { body } => {
                hircc::Stm::Block { body: body.iter().map(|e| e.convert()).collect() }
            },
            Stm::Eval { exp } => {
                hircc::Stm::Eval { exp: Box::new(exp.convert()) }
            },
            Stm::Assign { ty, lhs, rhs } => {
                hircc::Stm::Assign { ty: ty.clone(), lhs: *lhs, rhs: Box::new(rhs.convert()) }
            },
            Stm::ArrayAssign { bounds_check, ty, array, index, value } => {
                hircc::Stm::ArrayAssign { bounds_check: *bounds_check, ty: ty.clone(), array: Box::new(array.convert()), index: Box::new(index.convert()), value: Box::new(value.convert()) }
            },
            Stm::StructAssign { ty, base, field, value } => {
                hircc::Stm::StructAssign { ty: ty.clone(), base: Box::new(base.convert()), field: *field, value: Box::new(value.convert()) }
            },
        }
    }
}

pub trait LL<T> {
    fn lift(&self, decls: &mut Vec<Def>) -> T;
}

pub struct Lift;

impl Lift {
    pub fn lift(root: &Root) -> Root {
        let mut defs = Vec::new();
        let mut decls = Vec::new();

        for def in &root.defs {
            defs.push(def.lift(&mut decls));
        }

        defs.append(&mut decls);

        Root {
            defs
        }
    }
}

impl LL<Def> for Def {
    fn lift(&self, decls: &mut Vec<Def>) -> Def {
        match self {
            Def::VarDef { ty, name, exp } => {
                Def::VarDef { ty: ty.clone(), name: *name, exp: Box::new(exp.convert().lift(decls)) }
            },
            Def::FunDef { ret_type, name, params, body } => {
                Def::FunDef { ret_type: ret_type.clone(), name: *name, params: params.clone(), body: Box::new(body.convert().lift(decls)) }
            },
            Def::ExternDef { ret_type, name, params } => {
                Def::ExternDef { ret_type: ret_type.clone(), name: *name, params: params.clone() }
            }
        }
    }
}

impl LL<Exp> for hircc::Exp {
    fn lift(&self, decls: &mut Vec<Def>) -> Exp {
        match self {
            hircc::Exp::NewArray { ty, length } => {
                Exp::NewArray { ty: ty.clone(), length: Box::new(length.lift(decls)) }
            },
            hircc::Exp::ArrayLit { ty, exps } => {
                Exp::ArrayLit { ty: ty.clone(), exps: exps.iter().map(|e| e.lift(decls)).collect() }
            },
            hircc::Exp::ArrayLoad { bounds_check, ty, array, index } => {
                Exp::ArrayLoad { bounds_check: *bounds_check, ty: ty.clone(), array: Box::new(array.lift(decls)), index: Box::new(index.lift(decls)) }
            },
            hircc::Exp::ArrayLength { array } => {
                Exp::ArrayLength { array: Box::new(array.lift(decls)) }
            },
            hircc::Exp::Lit { lit } => {
                Exp::Lit { lit: lit.clone() }
            },
            hircc::Exp::Call { fun_type, name, args } => {
                Exp::Call { fun_type: fun_type.clone(), name: *name, args: args.iter().map(|e| e.lift(decls)).collect() }
            },
            hircc::Exp::Var { name, ty } => {
                Exp::Var { name: *name, ty: ty.clone() }
            },

            hircc::Exp::Binary { op, e1, e2 } => {
                Exp::Binary { op: *op, e1: Box::new(e1.lift(decls)), e2: Box::new(e2.lift(decls)) }
            },
            hircc::Exp::Unary { op, exp } => {
                Exp::Unary { op: *op, exp: Box::new(exp.lift(decls)) }
            },
            hircc::Exp::Box { ty, exp } => {
                Exp::Box { ty: ty.clone(), exp: Box::new(exp.lift(decls)) }
            },
            hircc::Exp::Unbox { ty, exp } => {
                Exp::Unbox { ty: ty.clone(), exp: Box::new(exp.lift(decls)) }
            },
            hircc::Exp::Cast { ty, exp } => {
                Exp::Cast { ty: ty.clone(), exp: Box::new(exp.lift(decls)) }
            },
            hircc::Exp::Seq { body, exp } => {
                Exp::Seq { body: Box::new(body.lift(decls)), exp: Box::new(exp.lift(decls)) }
            },
            hircc::Exp::Let {inits, body } => {
                Exp::Let { inits: inits.iter().map(|f| Field { param: f.param.clone(), exp: Box::new(f.exp.lift(decls)) }).collect(), body: Box::new(body.lift(decls)) }
            },
            hircc::Exp::LambdaCC { ret_type, env_param, params, body } => {
                let f = Name::fresh("lifted");

                // Add a parameter for the environment pointer.
                // The parameter type is just a void* (an empty struct pointer).
                let env_param_name = Name::fresh("env");
                let external_env_type = Type::Struct { fields: vec![] };

                let mut def_params = params.clone();
                def_params.push(Param {
                    ty: external_env_type.clone(),
                    name: env_param_name,
                });

                // Create the function type, using the opaque env pointer type.
                let mut args: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                args.push(external_env_type.clone());

                let fun_type = Type::Fun {
                    ret: Box::new(ret_type.clone()),
                    args: args
                };

                // Lift the body.
                let lifted_body = body.lift(decls);

                // Cast the env parameter to the more specific type, using the name
                // that was used for the env parameter during closure conversion.
                let env_ptr = Exp::Var { ty: external_env_type.clone(), name: env_param_name };
                let cast = Exp::Cast { ty: env_param.ty.clone(), exp: Box::new(env_ptr) };
                let exp = Exp::Let {
                    inits: vec![
                        Field {
                            param: env_param.clone(),
                            exp: Box::new(cast)
                        }
                    ],
                    body: Box::new(lifted_body),
                };

                // Declare the function using the new lifted body with cast.
                decls.push(Def::FunDef {
                    ret_type: ret_type.clone(),
                    name: f,
                    params: def_params.clone(),
                    body: Box::new(exp),
                });

                // Return a variable with the external function type.
                Exp::Var { name: f, ty: fun_type }
            },
            hircc::Exp::ApplyCC { fun_type, fun, args } => {
                // The caller doesn't know the environment type, just that it's a struct.
                let env_type = Type::Struct { fields: vec![] };

                let closure = Name::fresh("closure");
                let mut closure_args: Vec<Exp> = args.iter().map(|e| e.lift(decls)).collect();
                let closure_type = Type::Struct {
                    fields: vec![
                        Param { name: Name::new("fun"), ty: fun_type.clone() },
                        Param { name: Name::new("env"), ty: env_type.clone() } // TODO
                    ]
                };
                // Add environment at the end of the arguments.
                closure_args.push(
                    Exp::StructLoad {
                        ty: closure_type.clone(),
                        base: Box::new(Exp::Var { name: closure, ty: closure_type.clone() }),
                        field: Name::new("env"),
                    },
                );

                let cc_fun_type = match fun_type {
                    Type::Fun { ret, args } => {
                        let mut new_args = Vec::new();
                        for a in args {
                            new_args.push(a.clone());
                        }
                        new_args.push(env_type.clone());
                        Type::Fun { ret: ret.clone(), args: new_args }
                    },
                    _ => panic!("ApplyCC type should be a function type")
                };

                Exp::Let {
                    inits: vec![
                        Field {
                            param: Param { name: closure, ty: closure_type.clone() },
                            exp: Box::new(fun.lift(decls)),
                        }
                    ],
                    body: Box::new(
                        Exp::Apply {
                            fun_type: cc_fun_type,
                            fun: Box::new(
                                Exp::StructLoad {
                                    ty: closure_type.clone(),
                                    base: Box::new(Exp::Var { name: closure, ty: closure_type.clone() }),
                                    field: Name::new("fun"),
                                }
                            ),
                            args: closure_args
                        }
                    )
                }
            },
            hircc::Exp::StructLit { fields } => {
                Exp::StructLit {
                    fields: fields.iter().map(|f| Field { param: f.param.clone(), exp: Box::new(f.exp.lift(decls)) }).collect()
                 }
            },
            hircc::Exp::StructLoad { ty, base, field } => {
                Exp::StructLoad { ty: ty.clone(), base: Box::new(base.lift(decls)), field: *field }
            },
        }
    }
}

impl LL<Stm> for hircc::Stm {
    fn lift(&self, decls: &mut Vec<Def>) -> Stm {
        match self {
            hircc::Stm::IfElse { cond, if_true, if_false } => {
                Stm::IfElse { cond: Box::new(cond.lift(decls)), if_true: Box::new(if_true.lift(decls)), if_false: Box::new(if_false.lift(decls)) }
            },
            hircc::Stm::IfThen { cond, if_true } => {
                Stm::IfThen { cond: Box::new(cond.lift(decls)), if_true: Box::new(if_true.lift(decls)) }
            },
            hircc::Stm::While { cond, body } => {
                Stm::While { cond: Box::new(cond.lift(decls)), body: Box::new(body.lift(decls)) }
            },
            hircc::Stm::Return { exp } => {
                Stm::Return { exp: Box::new(exp.lift(decls)) }
            },
            hircc::Stm::Block { body } => {
                Stm::Block { body: body.iter().map(|e| e.lift(decls)).collect() }
            },
            hircc::Stm::Eval { exp } => {
                Stm::Eval { exp: Box::new(exp.lift(decls)) }
            },
            hircc::Stm::Assign { ty, lhs, rhs } => {
                Stm::Assign { ty: ty.clone(), lhs: *lhs, rhs: Box::new(rhs.lift(decls)) }
            },
            hircc::Stm::ArrayAssign { bounds_check, ty, array, index, value } => {
                Stm::ArrayAssign { bounds_check: *bounds_check, ty: ty.clone(), array: Box::new(array.lift(decls)), index: Box::new(index.lift(decls)), value: Box::new(value.lift(decls)) }
            },
            hircc::Stm::StructAssign { ty, base, field, value } => {
                Stm::StructAssign { ty: ty.clone(), base: Box::new(base.lift(decls)), field: *field, value: Box::new(value.lift(decls)) }
            },
        }
    }
}
