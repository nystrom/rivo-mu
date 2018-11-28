use std::collections::HashMap;
use std::collections::HashSet;

use crate::llvm;
use crate::common::names::*;
use crate::mir::trees as mir;
use crate::mir::ops::*;
use crate::lir::trees as lir;

#[allow(non_upper_case_globals)]
static mut depth: usize = 0;

macro_rules! intrinsic {
    ($self: expr, $name: expr, $v1: expr, ($ty1: expr) -> $retty: expr) => {
        $self.builder.call(
            $self.to_addr(&lir::Exp::Function {
                ty: mir::Type::Fun {
                    ret: Box::new($retty),
                    args: vec![$ty1]
                },
                name: Name::new($name)
            }),
            &[$v1],
            &$self.fresh_name())
    };
    ($self: expr, $name: expr, $v1: expr, $v2: expr, ($ty1: expr, $ty2: expr) -> $retty: expr) => {
        $self.builder.call(
            $self.to_addr(&lir::Exp::Function {
                ty: mir::Type::Fun {
                    ret: Box::new($retty),
                    args: vec![$ty1, $ty2]
                },
                name: Name::new($name)
            }),
            &[$v1, $v2],
            &$self.fresh_name())
    };
    ($self: expr, $name: expr, $v1: expr, $v2: expr, $v3: expr, ($ty1: expr, $ty2: expr, $ty3: expr) -> $retty: expr) => {
        $self.builder.call(
            $self.to_addr(&lir::Exp::Function {
                ty: mir::Type::Fun {
                    ret: Box::new($retty),
                    args: vec![$ty1, $ty2, $ty3]
                },
                name: Name::new($name)
            }),
            &[$v1, $v2, $v3],
            &$self.fresh_name())
    };
}


// 64-bit target
const WORDSIZE: usize = 8;

pub struct Translate {
    pub context: llvm::Context,
}

impl Translate {
    pub fn new() -> Translate {
        crate::llvm::init();
        Translate {
            context: llvm::Context::new(),
        }
    }

    pub fn new_in_context(context: llvm::Context) -> Translate {
        crate::llvm::init();
        Translate {
            context: context,
        }
    }

    pub fn translate(&self, name: &str, r: &lir::Root) -> llvm::Module {
        let builder = self.context.new_builder();
        let module = llvm::Module::new(name);

        let mut funs = Vec::new();

        for p in &r.procs {
            let t = ProcTranslator::new(&self.context, &module, &builder);
            let fun = t.init_proc(p);
            funs.push(fun);
        }

        for (p, fun) in r.procs.iter().zip(funs.iter()) {
            let t = ProcTranslator::new(&self.context, &module, &builder);
            t.translate_proc(p, *fun);
        }

        builder.dispose();
        module
    }

    fn to_type(context: &llvm::Context, ty: &lir::Type) -> llvm::Type {
        match ty {
            lir::Type::I1 => context.i1_type(),
            lir::Type::I32 => context.i32_type(),
            lir::Type::I64 => context.i64_type(),
            lir::Type::F32 => context.float_type(),
            lir::Type::F64 => context.double_type(),
            lir::Type::Word => if WORDSIZE == 8 { context.i64_type() } else { context.i32_type() },
            lir::Type::Void => context.void_type(),
            lir::Type::Ptr { ty } => {
                let t = Translate::to_type(context, ty);
                context.pointer_type(t)
            },
            lir::Type::Array { ty } => {
                let t = Translate::to_type(context, ty);
                let ps = vec![
                    Translate::to_type(context, &lir::Type::Word),
                    context.array_type(t, 0),
                ];
                context.structure_type(&ps, false)
            },
            lir::Type::Struct { fields } => {
                let ps: Vec<llvm::Type> = fields.iter().map(|a| Translate::to_type(context, a)).collect();
                context.structure_type(&ps, false)
            },
            lir::Type::Fun { ret, args } => {
                let r = Translate::to_type(context, ret);
                let ps: Vec<llvm::Type> = args.iter().map(|a| Translate::to_type(context, a)).collect();
                context.function_type(r, &ps, false)
            },
        }
    }
}

struct ProcTranslator<'a> {
    context: &'a llvm::Context,
    module: &'a llvm::Module,
    builder: &'a llvm::Builder,
}

struct BodyTranslator<'a> {
    context: &'a llvm::Context,
    module: &'a llvm::Module,
    builder: &'a llvm::Builder,
    fun: &'a llvm::Value,
    labels: HashMap<Name, llvm::BB>,
    temps: HashMap<Name, llvm::Value>, // maps from temp name to the alloca that created it.
    params: HashMap<Name, llvm::Value>, // maps from temp name to the alloca that created it.
}

impl<'a> ProcTranslator<'a> {
    fn new(context: &'a llvm::Context, module: &'a llvm::Module, builder: &'a llvm::Builder) -> Self {
        ProcTranslator { context, module, builder }
    }

    fn to_type(&self, ty: &lir::Type) -> llvm::Type {
        Translate::to_type(self.context, ty)
    }

    fn init_proc(&self, p: &lir::Proc) -> llvm::Value {
        let ty = self.to_type(&p.ret_type);
        let tys: Vec<llvm::Type> = p.params.iter().map(|p| self.to_type(&p.ty)).collect();
        let fun_ty = llvm::Type::function(ty, &tys, false);
        self.module.add_function(&p.name.to_string(), fun_ty)
    }

    fn translate_proc(&self, p: &lir::Proc, fun: llvm::Value) {
        let mut params = HashMap::new();

        for (i, p) in p.params.iter().enumerate() {
            params.insert(p.name, fun.get_param(i));
        }

        let mut t = BodyTranslator {
            context: &self.context,
            module: &self.module,
            builder: &self.builder,
            fun: &fun,
            labels: HashMap::new(),
            temps: HashMap::new(),
            params: params.clone(),
        };

        t.translate(&p.body);
    }
}

impl<'a> BodyTranslator<'a> {
    fn translate(&mut self, body: &Vec<lir::Stm>) {
        // Create the first BB.
        let entry = self.context.append_bb(self.fun.clone(), "entry");
        self.builder.position_at_end(entry);

        // Collect temporaries.
        let mut temps = HashSet::new();
        for s in body {
            TempFinder::add_temps_for_stm(s, &mut temps);
        }

        // Emit an alloca for each temporary, except params.
        for (x, xty) in &temps {
            if self.params.get(&x).is_some() {
                continue;
            }

            let ty = self.to_type(xty);
            let insn = self.builder.alloca(ty, &self.fresh_name());
            self.temps.insert(*x, insn.clone());
        }

        // Now, translate each statement.
        let mut last_was_jump = false;

        for s in body {
            match s {
                lir::Stm::Label { label } => {
                    let bb = self.to_bb(*label);

                    if ! last_was_jump {
                        // Jump from the previous BB to here if the last instruction
                        // of the previous BB was not a jump.
                        self.builder.br(bb);
                    }

                    self.builder.position_at_end(bb);
                },
                lir::Stm::Nop => {
                    // skip it.
                },
                _ => {
                    last_was_jump = match s {
                        lir::Stm::Jump { .. } => true,
                        lir::Stm::CJump { .. } => true,
                        lir::Stm::Ret { .. } => true,
                        _ => false,
                    };

                    self.translate_stm(s);
                }
            }
        }

        if ! last_was_jump {
            // If the last instruction was not a jump, add an unreachable insn.
            self.builder.unreachable();
        }
    }

    fn fresh_name(&self) -> String {
        Name::fresh("t.llvm").to_string()
    }

    fn to_value(&mut self, e: &lir::Exp) -> llvm::Value {
        match e {
            lir::Exp::Global { name, ty } => {
                let a = self.to_addr(e);
                let insn = self.builder.load(a, &self.fresh_name());
                insn
            },
            lir::Exp::Function { name, ty } => {
                let a = self.to_addr(e);
                let insn = self.builder.load(a, &self.fresh_name());
                insn
            },
            lir::Exp::Temp { name, ty } => {
                match self.params.get(&name) {
                    Some(v) => {
                        v.clone()
                    },
                    None => {
                        let a = self.to_addr(e);
                        let insn = self.builder.load(a, &self.fresh_name());
                        insn
                    }
                }
            },
            lir::Exp::Lit { lit: mir::Lit::I1 { value } } => {
                llvm::Value::i1(*value)
            },
            lir::Exp::Lit { lit: mir::Lit::I32 { value } } => {
                llvm::Value::i32(*value)
            },
            lir::Exp::Lit { lit: mir::Lit::I64 { value } } => {
                llvm::Value::i64(*value)
            },
            lir::Exp::Lit { lit: mir::Lit::F32 { value } } => {
                llvm::Value::float(*value)
            },
            lir::Exp::Lit { lit: mir::Lit::F64 { value } } => {
                llvm::Value::double(*value)
            },
            lir::Exp::Lit { lit: mir::Lit::Sizeof { ty } } => {
                // TODO
                if WORDSIZE == 4 {
                    llvm::Value::i32(WORDSIZE as i32)
                }
                else {
                    llvm::Value::i64(WORDSIZE as i64)
                }
            },
            lir::Exp::Lit { lit: mir::Lit::ArrayBaseOffset } => {
                if WORDSIZE == 4 {
                    llvm::Value::i32(WORDSIZE as i32)
                }
                else {
                    llvm::Value::i64(WORDSIZE as i64)
                }
            },
            lir::Exp::Lit { lit: mir::Lit::ArrayLengthOffset } => {
                if WORDSIZE == 4 {
                    llvm::Value::i32(0)
                }
                else {
                    llvm::Value::i64(0)
                }
            },
            lir::Exp::Lit { lit: mir::Lit::StructFieldOffset { ty, field} } => {
                // TODO
                if WORDSIZE == 4 {
                    llvm::Value::i32((*field * WORDSIZE) as i32)
                }
                else {
                    llvm::Value::i64((*field * WORDSIZE) as i64)
                }
            },
        }
    }

    fn to_addr(&mut self, e: &lir::Exp) -> llvm::Value {
        match e {
            lir::Exp::Global { name, ty } => {
                self.module.get_named_global(&name.to_string())
            },
            lir::Exp::Function { name, ty } => {
                self.module.get_named_function(&name.to_string())
            },
            lir::Exp::Temp { name, ty } => {
                match self.temps.get(&name) {
                    Some(v) => *v,
                    None => unreachable!(),  // shouldn't happen since we pre-filled the temps table.
                }
            },
            lir::Exp::Lit { lit } => {
                self.to_value(e)
            },
        }
    }

    fn to_bb(&mut self, label: Name) -> llvm::BB {
        match self.labels.get(&label) {
            Some(bb) => bb.clone(),
            None => {
                let bb = self.context.append_bb(self.fun.clone(), &label.to_string());
                self.labels.insert(label, bb);
                bb
            }
        }
    }

    fn to_type(&self, ty: &lir::Type) -> llvm::Type {
        Translate::to_type(self.context, ty)
    }

    fn translate_stm(&mut self, stm: &lir::Stm) {
        let insn = match stm {
            lir::Stm::CJump { cmp, if_true, if_false } => {
                let i = self.to_value(cmp);
                let t = self.to_bb(*if_true);
                let e = self.to_bb(*if_false);
                self.builder.cond_br(i, t, e)
            },
            lir::Stm::Jump { label } => {
                let l = self.to_bb(*label);
                self.builder.br(l)
            },
            lir::Stm::Ret { exp } => {
                let v = self.to_value(exp);
                self.builder.ret(v)
            },
            lir::Stm::Store { dst_addr, src } => {
                let v = self.to_value(src);
                let p = self.to_addr(dst_addr);
                self.builder.store(v, p)
            },
            lir::Stm::Load { dst, src_addr } => {
                let p = self.to_addr(src_addr);
                let v = self.builder.load(p, &self.fresh_name());
                let x = self.to_addr(dst);
                self.builder.store(v, x)
            },
            lir::Stm::Move { dst, src } => {
                let x = self.to_addr(dst);
                let v = self.to_value(src);
                self.builder.store(v, x)
            },
            lir::Stm::Call { dst, fun, args } => {
                let f = self.to_addr(fun);
                let vs: Vec<llvm::Value> = args.iter().map(|a| self.to_value(a)).collect();
                let v = self.builder.call(f, &vs, &self.fresh_name());
                let x = self.to_addr(dst);
                self.builder.store(v, x)
            },
            lir::Stm::Binary { dst, op, e1, e2 } => {
                let a1 = self.to_value(e1);
                let a2 = self.to_value(e2);
                let v = match op {
                    Bop::Add_i32 => self.builder.add(a1, a2, &self.fresh_name()),
                    Bop::Add_i64 => self.builder.add(a1, a2, &self.fresh_name()),
                    Bop::Add_f32 => self.builder.fadd(a1, a2, &self.fresh_name()),
                    Bop::Add_f64 => self.builder.fadd(a1, a2, &self.fresh_name()),

                    Bop::Sub_i32 => self.builder.sub(a1, a2, &self.fresh_name()),
                    Bop::Sub_i64 => self.builder.sub(a1, a2, &self.fresh_name()),
                    Bop::Sub_f32 => self.builder.fsub(a1, a2, &self.fresh_name()),
                    Bop::Sub_f64 => self.builder.fsub(a1, a2, &self.fresh_name()),

                    Bop::Mul_i32 => self.builder.mul(a1, a2, &self.fresh_name()),
                    Bop::Mul_i64 => self.builder.mul(a1, a2, &self.fresh_name()),
                    Bop::Mul_f32 => self.builder.fmul(a1, a2, &self.fresh_name()),
                    Bop::Mul_f64 => self.builder.fmul(a1, a2, &self.fresh_name()),

                    Bop::Div_s_i32 => self.builder.sdiv(a1, a2, &self.fresh_name()),
                    Bop::Div_s_i64 => self.builder.sdiv(a1, a2, &self.fresh_name()),
                    Bop::Div_u_i32 => self.builder.udiv(a1, a2, &self.fresh_name()),
                    Bop::Div_u_i64 => self.builder.udiv(a1, a2, &self.fresh_name()),
                    Bop::Div_f32 => self.builder.fdiv(a1, a2, &self.fresh_name()),
                    Bop::Div_f64 => self.builder.fdiv(a1, a2, &self.fresh_name()),

                    Bop::Rem_s_i32 => self.builder.srem(a1, a2, &self.fresh_name()),
                    Bop::Rem_s_i64 => self.builder.srem(a1, a2, &self.fresh_name()),
                    Bop::Rem_u_i32 => self.builder.urem(a1, a2, &self.fresh_name()),
                    Bop::Rem_u_i64 => self.builder.urem(a1, a2, &self.fresh_name()),
                    Bop::Rem_f32 => self.builder.frem(a1, a2, &self.fresh_name()),
                    Bop::Rem_f64 => self.builder.frem(a1, a2, &self.fresh_name()),

                    Bop::And_i32 => self.builder.and(a1, a2, &self.fresh_name()),
                    Bop::And_i64 => self.builder.and(a1, a2, &self.fresh_name()),

                    Bop::Or_i32 => self.builder.or(a1, a2, &self.fresh_name()),
                    Bop::Or_i64 => self.builder.or(a1, a2, &self.fresh_name()),

                    Bop::Xor_i32 => self.builder.xor(a1, a2, &self.fresh_name()),
                    Bop::Xor_i64 => self.builder.xor(a1, a2, &self.fresh_name()),

                    Bop::Shl_i32 => self.builder.shl(a1, a2, &self.fresh_name()),
                    Bop::Shl_i64 => self.builder.shl(a1, a2, &self.fresh_name()),

                    Bop::Shr_u_i32 => self.builder.lshr(a1, a2, &self.fresh_name()),
                    Bop::Shr_u_i64 => self.builder.lshr(a1, a2, &self.fresh_name()),

                    Bop::Shr_i32 => self.builder.ashr(a1, a2, &self.fresh_name()),
                    Bop::Shr_i64 => self.builder.ashr(a1, a2, &self.fresh_name()),

                    Bop::Eq_ptr => self.builder.icmp(llvm::IntPredicate::EQ, a1, a2, &self.fresh_name()),
                    Bop::Eq_z => self.builder.icmp(llvm::IntPredicate::EQ, a1, a2, &self.fresh_name()),
                    Bop::Eq_i32 => self.builder.icmp(llvm::IntPredicate::EQ, a1, a2, &self.fresh_name()),
                    Bop::Eq_i64 => self.builder.icmp(llvm::IntPredicate::EQ, a1, a2, &self.fresh_name()),
                    Bop::Eq_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedEQ, a1, a2, &self.fresh_name()),
                    Bop::Eq_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedEQ, a1, a2, &self.fresh_name()),

                    Bop::Ne_ptr => self.builder.icmp(llvm::IntPredicate::NE, a1, a2, &self.fresh_name()),
                    Bop::Ne_z => self.builder.icmp(llvm::IntPredicate::NE, a1, a2, &self.fresh_name()),
                    Bop::Ne_i32 => self.builder.icmp(llvm::IntPredicate::NE, a1, a2, &self.fresh_name()),
                    Bop::Ne_i64 => self.builder.icmp(llvm::IntPredicate::NE, a1, a2, &self.fresh_name()),
                    Bop::Ne_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedNE, a1, a2, &self.fresh_name()),
                    Bop::Ne_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedNE, a1, a2, &self.fresh_name()),

                    Bop::Lt_s_i32 => self.builder.icmp(llvm::IntPredicate::SignedLT, a1, a2, &self.fresh_name()),
                    Bop::Lt_s_i64 => self.builder.icmp(llvm::IntPredicate::SignedLT, a1, a2, &self.fresh_name()),
                    Bop::Lt_u_i32 => self.builder.icmp(llvm::IntPredicate::UnsignedLT, a1, a2, &self.fresh_name()),
                    Bop::Lt_u_i64 => self.builder.icmp(llvm::IntPredicate::UnsignedLT, a1, a2, &self.fresh_name()),
                    Bop::Lt_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedLT, a1, a2, &self.fresh_name()),
                    Bop::Lt_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedLT, a1, a2, &self.fresh_name()),

                    Bop::Le_s_i32 => self.builder.icmp(llvm::IntPredicate::SignedLE, a1, a2, &self.fresh_name()),
                    Bop::Le_s_i64 => self.builder.icmp(llvm::IntPredicate::SignedLE, a1, a2, &self.fresh_name()),
                    Bop::Le_u_i32 => self.builder.icmp(llvm::IntPredicate::UnsignedLE, a1, a2, &self.fresh_name()),
                    Bop::Le_u_i64 => self.builder.icmp(llvm::IntPredicate::UnsignedLE, a1, a2, &self.fresh_name()),
                    Bop::Le_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedLE, a1, a2, &self.fresh_name()),
                    Bop::Le_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedLE, a1, a2, &self.fresh_name()),

                    Bop::Gt_s_i32 => self.builder.icmp(llvm::IntPredicate::SignedGT, a1, a2, &self.fresh_name()),
                    Bop::Gt_s_i64 => self.builder.icmp(llvm::IntPredicate::SignedGT, a1, a2, &self.fresh_name()),
                    Bop::Gt_u_i32 => self.builder.icmp(llvm::IntPredicate::UnsignedGT, a1, a2, &self.fresh_name()),
                    Bop::Gt_u_i64 => self.builder.icmp(llvm::IntPredicate::UnsignedGT, a1, a2, &self.fresh_name()),
                    Bop::Gt_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedGT, a1, a2, &self.fresh_name()),
                    Bop::Gt_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedGT, a1, a2, &self.fresh_name()),

                    Bop::Ge_s_i32 => self.builder.icmp(llvm::IntPredicate::SignedGE, a1, a2, &self.fresh_name()),
                    Bop::Ge_s_i64 => self.builder.icmp(llvm::IntPredicate::SignedGE, a1, a2, &self.fresh_name()),
                    Bop::Ge_u_i32 => self.builder.icmp(llvm::IntPredicate::UnsignedGE, a1, a2, &self.fresh_name()),
                    Bop::Ge_u_i64 => self.builder.icmp(llvm::IntPredicate::UnsignedGE, a1, a2, &self.fresh_name()),
                    Bop::Ge_f32 => self.builder.fcmp(llvm::RealPredicate::OrderedGE, a1, a2, &self.fresh_name()),
                    Bop::Ge_f64 => self.builder.fcmp(llvm::RealPredicate::OrderedGE, a1, a2, &self.fresh_name()),

                    Bop::Min_f32 => intrinsic!(self, "llvm.minimum.f32", a1, a2, (mir::Type::F32, mir::Type::F32) -> mir::Type::F32),
                    Bop::Min_f64 => intrinsic!(self, "llvm.minimum.f64", a1, a2, (mir::Type::F64, mir::Type::F64) -> mir::Type::F64),
                    Bop::Max_f32 => intrinsic!(self, "llvm.maximum.f32", a1, a2, (mir::Type::F32, mir::Type::F32) -> mir::Type::F32),
                    Bop::Max_f64 => intrinsic!(self, "llvm.maximum.f64", a1, a2, (mir::Type::F64, mir::Type::F64) -> mir::Type::F64),

                    Bop::Copysign_f32 => intrinsic!(self, "llvm.copysign.f32", a1, a2, (mir::Type::F32, mir::Type::F32) -> mir::Type::F32),
                    Bop::Copysign_f64 => intrinsic!(self, "llvm.copysign.f64", a1, a2, (mir::Type::F64, mir::Type::F64) -> mir::Type::F64),

                    Bop::Add_word => self.builder.add(a1, a2, &self.fresh_name()),
                    Bop::Mul_word => self.builder.mul(a1, a2, &self.fresh_name()),

                    // And and or can be implemented with bitwise operations.
                    // These don't short-circuit, but since the subexpressions have no structure, it should be okay.
                    Bop::And_z => self.builder.and(a1, a2, &self.fresh_name()),
                    Bop::Or_z => self.builder.or(a1, a2, &self.fresh_name()),

                    // Use the funnel shift intrinsics to perform rotations.
                    // The first two arguments are the same.
                    Bop::Rotl_i32 => intrinsic!(self, "llvm.fshl.i32", a1, a1, a2, (mir::Type::I32, mir::Type::I32, mir::Type::I32) -> mir::Type::I32),
                    Bop::Rotl_i64 => intrinsic!(self, "llvm.fshl.i64", a1, a1, a2, (mir::Type::I64, mir::Type::I64, mir::Type::I64) -> mir::Type::I64),
                    Bop::Rotr_i32 => intrinsic!(self, "llvm.fshr.i32", a1, a1, a2, (mir::Type::I32, mir::Type::I32, mir::Type::I32) -> mir::Type::I32),
                    Bop::Rotr_i64 => intrinsic!(self, "llvm.fshr.i64", a1, a1, a2, (mir::Type::I64, mir::Type::I64, mir::Type::I64) -> mir::Type::I64),

                    Bop::Atan2_f32 => unimplemented!(),
                    Bop::Atan2_f64 => unimplemented!(),
                };
                let x = self.to_addr(dst);
                self.builder.store(v, x)
            },
            lir::Stm::Unary { dst, op, exp } => {
                let e = self.to_value(exp);
                let v = match op {
                    Uop::Not_z => self.builder.not(e, &self.fresh_name()),
                    Uop::Neg_f32 => self.builder.fneg(e, &self.fresh_name()),
                    Uop::Neg_f64 => self.builder.fneg(e, &self.fresh_name()),

                    Uop::Ctz_i32 => intrinsic!(self, "llvm.cttz.i32", e, (mir::Type::I32) -> mir::Type::I32),
                    Uop::Clz_i32 => intrinsic!(self, "llvm.ctlz.i32", e, (mir::Type::I32) -> mir::Type::I32),
                    Uop::Popcount_i32 => intrinsic!(self, "llvm.ctpop.i32", e, (mir::Type::I32) -> mir::Type::I32),
                    Uop::Eqz_i32 => self.builder.icmp(llvm::IntPredicate::EQ, e, llvm::Value::i32(0), &self.fresh_name()),
                    Uop::Complement_i32 => self.builder.xor(e, llvm::Value::i32(-1), &self.fresh_name()),

                    Uop::Ctz_i64 => intrinsic!(self, "llvm.cttz.i64", e, (mir::Type::I64) -> mir::Type::I64),
                    Uop::Clz_i64 => intrinsic!(self, "llvm.ctlz.i64", e, (mir::Type::I64) -> mir::Type::I64),
                    Uop::Popcount_i64 => intrinsic!(self, "llvm.ctpop.i64", e, (mir::Type::I64) -> mir::Type::I64),
                    Uop::Eqz_i64 => self.builder.icmp(llvm::IntPredicate::EQ, e, llvm::Value::i64(0), &self.fresh_name()),
                    Uop::Complement_i64 => self.builder.xor(e, llvm::Value::i64(-1), &self.fresh_name()),

                    Uop::Abs_f32 => intrinsic!(self, "llvm.fabs.f32", e, (mir::Type::F32) -> mir::Type::F32),

                    Uop::Ceil_f32 => intrinsic!(self, "llvm.ceil.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Floor_f32 => intrinsic!(self, "llvm.floor.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Trunc_f32 => intrinsic!(self, "llvm.trunc.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Nearest_f32 => intrinsic!(self, "llvm.round.f32", e, (mir::Type::F32) -> mir::Type::F32),

                    Uop::Exp_f32 => intrinsic!(self, "llvm.exp.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Log_f32 => intrinsic!(self, "llvm.log.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Sqrt_f32 => intrinsic!(self, "llvm.sqrt.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Pow_f32 => intrinsic!(self, "llvm.pow.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Logb_f32 => unimplemented!(),
                    Uop::Sin_f32 => intrinsic!(self, "llvm.sin.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Cos_f32 => intrinsic!(self, "llvm.cos.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Tan_f32 => intrinsic!(self, "llvm.tan.f32", e, (mir::Type::F32) -> mir::Type::F32),
                    Uop::Asin_f32 => unimplemented!(),
                    Uop::Acos_f32 => unimplemented!(),
                    Uop::Atan_f32 => unimplemented!(),
                    Uop::Sinh_f32 => unimplemented!(),
                    Uop::Cosh_f32 => unimplemented!(),
                    Uop::Tanh_f32 => unimplemented!(),
                    Uop::Asinh_f32 => unimplemented!(),
                    Uop::Acosh_f32 => unimplemented!(),
                    Uop::Atanh_f32 => unimplemented!(),

                    Uop::IsNan_f32 => unimplemented!(),
                    Uop::IsInf_f32 => unimplemented!(),
                    Uop::IsDenormalized_f32 => unimplemented!(),
                    Uop::IsNegativeZero_f32 => unimplemented!(),
                    Uop::IsIEEE_f32 => unimplemented!(),

                    Uop::Abs_f64 => intrinsic!(self, "llvm.fabs.f64", e, (mir::Type::F64) -> mir::Type::F64),

                    Uop::Ceil_f64 => intrinsic!(self, "llvm.ceil.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Floor_f64 => intrinsic!(self, "llvm.floor.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Trunc_f64 => intrinsic!(self, "llvm.trunc.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Nearest_f64 => intrinsic!(self, "llvm.round.f64", e, (mir::Type::F64) -> mir::Type::F64),

                    Uop::Exp_f64 => intrinsic!(self, "llvm.exp.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Log_f64 => intrinsic!(self, "llvm.log.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Sqrt_f64 => intrinsic!(self, "llvm.sqrt.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Pow_f64 => intrinsic!(self, "llvm.pos.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Logb_f64 => unimplemented!(),
                    Uop::Sin_f64 => intrinsic!(self, "llvm.sin.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Cos_f64 => intrinsic!(self, "llvm.cos.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Tan_f64 => intrinsic!(self, "llvm.tan.f64", e, (mir::Type::F64) -> mir::Type::F64),
                    Uop::Asin_f64 => unimplemented!(),
                    Uop::Acos_f64 => unimplemented!(),
                    Uop::Atan_f64 => unimplemented!(),
                    Uop::Sinh_f64 => unimplemented!(),
                    Uop::Cosh_f64 => unimplemented!(),
                    Uop::Tanh_f64 => unimplemented!(),
                    Uop::Asinh_f64 => unimplemented!(),
                    Uop::Acosh_f64 => unimplemented!(),
                    Uop::Atanh_f64 => unimplemented!(),

                    Uop::IsNan_f64 => unimplemented!(),
                    Uop::IsInf_f64 => unimplemented!(),
                    Uop::IsDenormalized_f64 => unimplemented!(),
                    Uop::IsNegativeZero_f64 => unimplemented!(),
                    Uop::IsIEEE_f64 => unimplemented!(),

                    Uop::Wrap_i64_i32 => self.builder.trunc(e, self.to_type(&mir::Type::I32), &self.fresh_name()),

                    Uop::Trunc_s_f32_i32 => self.builder.fp_to_si(e, self.to_type(&mir::Type::I32), &self.fresh_name()),
                    Uop::Trunc_s_f64_i32 => self.builder.fp_to_si(e, self.to_type(&mir::Type::I32), &self.fresh_name()),
                    Uop::Trunc_u_f32_i32 => self.builder.fp_to_ui(e, self.to_type(&mir::Type::I32), &self.fresh_name()),
                    Uop::Trunc_u_f64_i32 => self.builder.fp_to_ui(e, self.to_type(&mir::Type::I32), &self.fresh_name()),
                    Uop::Trunc_s_f32_i64 => self.builder.fp_to_si(e, self.to_type(&mir::Type::I64), &self.fresh_name()),
                    Uop::Trunc_s_f64_i64 => self.builder.fp_to_si(e, self.to_type(&mir::Type::I64), &self.fresh_name()),
                    Uop::Trunc_u_f32_i64 => self.builder.fp_to_ui(e, self.to_type(&mir::Type::I64), &self.fresh_name()),
                    Uop::Trunc_u_f64_i64 => self.builder.fp_to_ui(e, self.to_type(&mir::Type::I64), &self.fresh_name()),

                    Uop::Extend_s_i32_i64 => self.builder.sext(e, self.to_type(&mir::Type::I64), &self.fresh_name()),
                    Uop::Extend_u_i32_i64 => self.builder.zext(e, self.to_type(&mir::Type::I64), &self.fresh_name()),

                    Uop::Reinterpret_f32_i32 => self.builder.bitcast(e, self.to_type(&mir::Type::I32), &self.fresh_name()),
                    Uop::Reinterpret_f64_i64 => self.builder.bitcast(e, self.to_type(&mir::Type::I64), &self.fresh_name()),
                    Uop::Reinterpret_i32_f32 => self.builder.bitcast(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Reinterpret_i64_f64 => self.builder.bitcast(e, self.to_type(&mir::Type::F64), &self.fresh_name()),

                    Uop::Convert_s_i32_f32 => self.builder.si_to_fp(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Convert_u_i32_f32 => self.builder.ui_to_fp(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Convert_s_i64_f32 => self.builder.si_to_fp(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Convert_u_i64_f32 => self.builder.ui_to_fp(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Convert_s_i32_f64 => self.builder.si_to_fp(e, self.to_type(&mir::Type::F64), &self.fresh_name()),
                    Uop::Convert_u_i32_f64 => self.builder.ui_to_fp(e, self.to_type(&mir::Type::F64), &self.fresh_name()),
                    Uop::Convert_s_i64_f64 => self.builder.si_to_fp(e, self.to_type(&mir::Type::F64), &self.fresh_name()),
                    Uop::Convert_u_i64_f64 => self.builder.ui_to_fp(e, self.to_type(&mir::Type::F64), &self.fresh_name()),

                    Uop::Demote_f64_f32 => self.builder.fptrunc(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                    Uop::Promote_f32_f64 => self.builder.fpext(e, self.to_type(&mir::Type::F32), &self.fresh_name()),
                };

                let x = self.to_addr(dst);
                self.builder.store(v, x)
            },
            lir::Stm::Cast { dst, ty, exp } => {
                let t = self.to_type(ty);
                let e = self.to_value(exp);
                let v = self.builder.bitcast(e, t, &self.fresh_name());
                let x = self.to_addr(dst);
                self.builder.store(v, x)
            },
            lir::Stm::GetStructElementAddr { dst, struct_ty, ptr, field: usize } => {
                unimplemented!()
            },
            lir::Stm::GetArrayElementAddr { dst, base_ty, ptr, index } => {
                unimplemented!()
            },
            lir::Stm::GetArrayLengthAddr { dst, ptr } => {
                unimplemented!()
            },

            // These should be handled by the caller.
            lir::Stm::Nop => {
                unreachable!()
            },
            lir::Stm::Label { label } => {
                unreachable!()
            },
        };
    }
}

struct TempFinder;

impl TempFinder {
    fn add_temps_for_exp(e: &lir::Exp, temps: &mut HashSet<(Name, mir::Type)>) {
        match e {
            lir::Exp::Global { name, ty } => {},
            lir::Exp::Function { name, ty } => {},
            lir::Exp::Temp { name, ty } => { temps.insert((*name, ty.clone())); }
            lir::Exp::Lit { lit } => {},
        }
    }

    fn add_temps_for_stm(s: &lir::Stm, temps: &mut HashSet<(Name, mir::Type)>) {
        match s {
            lir::Stm::Nop => {},
            lir::Stm::CJump { cmp, if_true, if_false } => {
                TempFinder::add_temps_for_exp(cmp, temps);
            },
            lir::Stm::Jump { label } => {},
            lir::Stm::Ret { exp } => {
                TempFinder::add_temps_for_exp(exp, temps);
            },
            lir::Stm::Store { dst_addr, src } => {
                TempFinder::add_temps_for_exp(dst_addr, temps);
                TempFinder::add_temps_for_exp(src, temps);
            },
            lir::Stm::Load { dst, src_addr } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(src_addr, temps);
            },
            lir::Stm::Move { dst, src } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(src, temps);
            },
            lir::Stm::Call { dst, fun, args } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(fun, temps);
                for arg in args {
                    TempFinder::add_temps_for_exp(arg, temps);
                }
            },
            lir::Stm::Binary { dst, op, e1, e2 } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(e1, temps);
                TempFinder::add_temps_for_exp(e2, temps);
            },
            lir::Stm::Unary { dst, op, exp } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(exp, temps);
            },
            lir::Stm::Cast { dst, ty, exp } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(exp, temps);
            },
            lir::Stm::Label { label } => {},
            lir::Stm::GetStructElementAddr { dst, struct_ty, ptr, field: usize } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(ptr, temps);
            },
            lir::Stm::GetArrayElementAddr { dst, base_ty, ptr, index } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(ptr, temps);
                TempFinder::add_temps_for_exp(index, temps);
            },
            lir::Stm::GetArrayLengthAddr { dst, ptr } => {
                TempFinder::add_temps_for_exp(dst, temps);
                TempFinder::add_temps_for_exp(ptr, temps);
            },
        }
    }
}
