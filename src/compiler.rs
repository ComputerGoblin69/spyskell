use crate::{
    ir::{BinLogicOp, BinMathOp, Comparison, Instruction},
    ssa::{self, Op},
    typ::{FunctionSignature, Type},
};
use anyhow::Result;
use cranelift::prelude::{
    codegen::{
        ir::{Function, Inst, UserFuncName},
        Context,
    },
    isa::TargetIsa,
    settings,
    types::{F32, I32, I8},
    AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext,
    InstBuilder, IntCC, MemFlags, Signature, StackSlotData, StackSlotKind,
    Value,
};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::{collections::HashMap, fs::File, io::Write, path::Path};

pub struct Options<'a> {
    pub target_triple: &'a str,
    pub out_path: &'a Path,
}

pub fn compile(
    program: crate::typ::CheckedProgram,
    options: &Options,
) -> Result<()> {
    let mut shared_builder = settings::builder();
    shared_builder.enable("is_pic")?;
    shared_builder.set("opt_level", "speed_and_size")?;

    let shared_flags = settings::Flags::new(shared_builder);
    let isa = cranelift::codegen::isa::lookup_by_name(options.target_triple)?
        .finish(shared_flags)?;
    let extern_function_signatures = extern_function_signatures(&*isa);

    let function_signatures = program
        .functions
        .iter()
        .map(|(name, function)| (name.clone(), function.signature.clone()))
        .collect();
    let value_generator = ssa::ValueGenerator::default();

    let object_builder = ObjectBuilder::new(
        isa.clone(),
        [],
        cranelift_module::default_libcall_names(),
    )?;
    let mut object_module = ObjectModule::new(object_builder);

    let clif_function_signatures = program
        .functions
        .iter()
        .map(|(name, function)| {
            (name.clone(), function.signature.to_clif(name, &*isa))
        })
        .collect::<HashMap<_, _>>();
    let function_ids = clif_function_signatures
        .iter()
        .map(|(name, signature)| {
            let func_id = if name == "main" {
                object_module.declare_function(
                    "main",
                    Linkage::Export,
                    signature,
                )
            } else {
                object_module.declare_anonymous_function(signature)
            }
            .unwrap();
            (name.clone(), func_id)
        })
        .collect();

    let mut compiler = Compiler {
        function_ids,
        function_signatures,
        clif_function_signatures,
        value_generator,
        ssa_values: HashMap::new(),
        isa: &*isa,
        object_module,
        extern_functions: HashMap::new(),
        extern_function_signatures,
    };
    compiler.compile(program)?;

    let object_bytes = compiler.object_module.finish().emit()?;
    let mut object_file = File::create(options.out_path)?;
    object_file.write_all(&object_bytes)?;

    Ok(())
}

struct Compiler<'a> {
    function_signatures: HashMap<String, FunctionSignature>,
    clif_function_signatures: HashMap<String, Signature>,
    function_ids: HashMap<String, FuncId>,
    value_generator: ssa::ValueGenerator,
    ssa_values: HashMap<ssa::Value, Value>,
    isa: &'a dyn TargetIsa,
    object_module: ObjectModule,
    extern_functions: HashMap<&'static str, FuncId>,
    extern_function_signatures: HashMap<&'static str, Signature>,
}

impl Compiler<'_> {
    fn take(&mut self, value: ssa::Value) -> Value {
        self.ssa_values.remove(&value).unwrap()
    }

    fn set(&mut self, value: ssa::Value, clif_value: Value) {
        self.ssa_values.insert(value, clif_value);
    }

    fn call_extern(
        &mut self,
        func_name: &'static str,
        args: &[Value],
        fb: &mut FunctionBuilder,
    ) -> Inst {
        let func_id =
            *self.extern_functions.entry(func_name).or_insert_with(|| {
                let Some(signature) = self.extern_function_signatures.get(func_name) else {
                    panic!("extern function `{func_name}` missing signature");
                };
                self.object_module
                    .declare_function(func_name, Linkage::Import, signature)
                    .unwrap()
            });
        let func_ref =
            self.object_module.declare_func_in_func(func_id, fb.func);
        fb.ins().call(func_ref, args)
    }

    fn compile(&mut self, program: crate::typ::CheckedProgram) -> Result<()> {
        let mut ctx = Context::new();
        let mut func_ctx = FunctionBuilderContext::new();

        for (name, function) in program.functions {
            self.compile_function(&name, function, &mut ctx, &mut func_ctx)?;
        }

        Ok(())
    }

    fn compile_function(
        &mut self,
        name: &str,
        function: crate::typ::CheckedFunction,
        ctx: &mut Context,
        func_ctx: &mut FunctionBuilderContext,
    ) -> Result<()> {
        let signature = self.clif_function_signatures[name].clone();
        let input_count = signature.params.len().try_into().unwrap();
        let func_id = self.function_ids[name];
        ctx.clear();
        ctx.func =
            Function::with_name_signature(UserFuncName::default(), signature);

        let mut graph = ssa::Graph::from_block(
            function.body,
            input_count,
            &self.function_signatures,
            &mut self.value_generator,
        );
        if std::env::var_os("SPACKEL_PRINT_SSA").is_some() {
            eprintln!("{name}: {graph:#?}");
        }

        let mut fb = FunctionBuilder::new(&mut ctx.func, func_ctx);
        let block = fb.create_block();
        fb.append_block_params_for_function_params(block);
        for (&ssa_value, &param) in
            std::iter::zip(&graph.inputs, fb.block_params(block))
        {
            self.set(ssa_value, param);
        }
        fb.switch_to_block(block);
        fb.seal_block(block);

        for assignment in graph.assignments {
            self.compile_assignment(assignment, &mut fb);
        }

        if name == "main" {
            let exit_code = self.value_generator.new_value();
            self.set(exit_code, fb.ins().iconst(I32, 0));
            graph.outputs.push(exit_code);
        }
        fb.ins().return_(
            &graph
                .outputs
                .iter()
                .map(|output| self.ssa_values[output])
                .collect::<Vec<_>>(),
        );

        fb.finalize();
        self.object_module.define_function(func_id, ctx)?;

        Ok(())
    }

    fn compile_assignment(
        &mut self,
        ssa::Assignment { to, args, op }: ssa::Assignment,
        fb: &mut FunctionBuilder,
    ) {
        match op {
            Op::Ins((Instruction::Call(name), _)) => {
                let func_id = self.function_ids[&*name];
                let func_ref =
                    self.object_module.declare_func_in_func(func_id, fb.func);
                let call_args =
                    args.iter().map(|&arg| self.take(arg)).collect::<Vec<_>>();
                let inst = fb.ins().call(func_ref, &call_args);
                for (value, &res) in std::iter::zip(&to, fb.inst_results(inst))
                {
                    self.set(value, res);
                }
            }
            Op::Then(body) => self.compile_then(to, &args, *body, fb),
            Op::ThenElse(then, else_) => {
                self.compile_then_else(to, &args, *then, *else_, fb);
            }
            Op::Repeat(body) => self.compile_repeat(to, &args, *body, fb),
            Op::Dup => {
                let v = self.take(args[0]);
                self.ssa_values.insert(to + 0, v);
                self.ssa_values.insert(to + 1, v);
            }
            Op::Drop => {
                self.take(args[0]);
            }
            Op::Ins((Instruction::PushI32(number), _)) => {
                self.set(to + 0, fb.ins().iconst(I32, i64::from(number)));
            }
            Op::Ins((Instruction::PushF32(number), _)) => {
                self.set(to + 0, fb.ins().f32const(number));
            }
            Op::Ins((Instruction::PushBool(b), _)) => {
                self.set(to + 0, fb.ins().iconst(I8, i64::from(b)));
            }
            Op::Ins((Instruction::PushType(_) | Instruction::TypeOf, _)) => {
                todo!();
            }
            Op::Ins((Instruction::Print, generics)) => {
                let n = self.take(args[0]);
                self.call_extern(
                    if generics[0] == Type::F32 {
                        "spkl_print_f32"
                    } else {
                        "spkl_print_i32"
                    },
                    &[n],
                    fb,
                );
            }
            Op::Ins((Instruction::Println, generics)) => {
                let n = self.take(args[0]);
                self.call_extern(
                    if generics[0] == Type::F32 {
                        "spkl_println_f32"
                    } else {
                        "spkl_println_i32"
                    },
                    &[n],
                    fb,
                );
            }
            Op::Ins((Instruction::PrintChar, _)) => {
                let n = self.take(args[0]);
                self.call_extern("spkl_print_char", &[n], fb);
            }
            Op::Ins((Instruction::BinMathOp(op), generics)) => {
                let a = self.take(args[0]);
                let b = self.take(args[1]);
                self.set(
                    to + 0,
                    match (generics.first(), op) {
                        (Some(Type::F32), BinMathOp::Add) => {
                            fb.ins().fadd(a, b)
                        }
                        (Some(Type::F32), BinMathOp::Sub) => {
                            fb.ins().fsub(a, b)
                        }
                        (Some(Type::F32), BinMathOp::Mul) => {
                            fb.ins().fmul(a, b)
                        }
                        (Some(Type::F32), BinMathOp::Div) => {
                            fb.ins().fdiv(a, b)
                        }
                        (_, BinMathOp::Add) => fb.ins().iadd(a, b),
                        (_, BinMathOp::Sub) => fb.ins().isub(a, b),
                        (_, BinMathOp::Mul) => fb.ins().imul(a, b),
                        (_, BinMathOp::Div) => fb.ins().sdiv(a, b),
                        (_, BinMathOp::Rem) => fb.ins().srem(a, b),
                        (_, BinMathOp::SillyAdd) => todo!(),
                    },
                );
            }
            Op::Ins((Instruction::Sqrt, _)) => {
                let n = self.take(args[0]);
                self.set(to + 0, fb.ins().sqrt(n));
            }
            Op::Ins((Instruction::Comparison(comparison), _)) => {
                let a = self.take(args[0]);
                let b = self.take(args[1]);
                self.set(
                    to + 0,
                    fb.ins().icmp(
                        match comparison {
                            Comparison::Lt => IntCC::SignedLessThan,
                            Comparison::Le => IntCC::SignedLessThanOrEqual,
                            Comparison::Eq => IntCC::Equal,
                            Comparison::Ge => IntCC::SignedGreaterThanOrEqual,
                            Comparison::Gt => IntCC::SignedGreaterThan,
                        },
                        a,
                        b,
                    ),
                );
            }
            Op::Ins((Instruction::Not, _)) => {
                let b = self.take(args[0]);
                self.set(to + 0, fb.ins().bxor_imm(b, 1));
            }
            Op::Ins((Instruction::BinLogicOp(op), _)) => {
                let a = self.take(args[0]);
                let b = self.take(args[1]);
                self.set(
                    to + 0,
                    match op {
                        BinLogicOp::And => fb.ins().band(a, b),
                        BinLogicOp::Or => fb.ins().bor(a, b),
                        BinLogicOp::Xor => fb.ins().bxor(a, b),
                        BinLogicOp::Nand => {
                            let res = fb.ins().band(a, b);
                            fb.ins().bxor_imm(res, 1)
                        }
                        BinLogicOp::Nor => {
                            let res = fb.ins().bor(a, b);
                            fb.ins().bxor_imm(res, 1)
                        }
                        BinLogicOp::Xnor => {
                            let res = fb.ins().bxor(a, b);
                            fb.ins().bxor_imm(res, 1)
                        }
                    },
                );
            }
            Op::Ins((Instruction::AddrOf, generics)) => {
                let typ = generics[0].to_clif(self.isa).unwrap();
                let stack_slot = fb.create_sized_stack_slot(StackSlotData {
                    kind: StackSlotKind::ExplicitSlot,
                    size: typ.bytes(),
                });
                let v = self.take(args[0]);
                self.set(to + 0, v);
                fb.ins().stack_store(v, stack_slot, 0);
                self.set(
                    to + 1,
                    fb.ins().stack_addr(self.isa.pointer_type(), stack_slot, 0),
                );
            }
            Op::Ins((Instruction::ReadPtr, generics)) => {
                let ptr = self.take(args[0]);
                let typ = generics[0].to_clif(self.isa).unwrap();
                self.set(
                    to + 0,
                    fb.ins().load(typ, MemFlags::trusted(), ptr, 0),
                );
            }
            Op::Ins((
                Instruction::Then(..)
                | Instruction::ThenElse(..)
                | Instruction::Repeat { .. }
                | Instruction::Unsafe(..)
                | Instruction::Dup
                | Instruction::Drop
                | Instruction::Swap
                | Instruction::Nip
                | Instruction::Tuck
                | Instruction::Over,
                _,
            )) => unreachable!(),
        }
    }

    fn compile_then(
        &mut self,
        to: ssa::ValueSequence,
        args: &[ssa::Value],
        body: ssa::Graph,
        fb: &mut FunctionBuilder,
    ) {
        let (&condition, args) = args.split_last().unwrap();

        for (&arg, &input) in std::iter::zip(args, &body.inputs) {
            let clif_value = self.take(arg);
            self.set(input, clif_value);
        }

        let then = fb.create_block();
        let after = fb.create_block();

        let condition = self.take(condition);
        fb.ins().brif(
            condition,
            then,
            &[],
            after,
            &args
                .iter()
                .map(|arg| self.ssa_values[arg])
                .collect::<Vec<_>>(),
        );
        fb.seal_block(then);

        fb.switch_to_block(then);
        for assignment in body.assignments {
            self.compile_assignment(assignment, fb);
        }
        for (value, out) in std::iter::zip(&to, &body.outputs) {
            self.set(
                value,
                fb.append_block_param(
                    after,
                    fb.func.dfg.value_type(self.ssa_values[out]),
                ),
            );
        }
        fb.ins().jump(
            after,
            &body
                .outputs
                .iter()
                .map(|&out| self.take(out))
                .collect::<Vec<_>>(),
        );
        fb.seal_block(after);

        fb.switch_to_block(after);
    }

    fn compile_then_else(
        &mut self,
        to: ssa::ValueSequence,
        args: &[ssa::Value],
        then: ssa::Graph,
        else_: ssa::Graph,
        fb: &mut FunctionBuilder,
    ) {
        let (&condition, args) = args.split_last().unwrap();

        for (arg, &input) in std::iter::zip(args, &then.inputs) {
            self.set(input, self.ssa_values[arg]);
        }
        for (&arg, &input) in std::iter::zip(args, &else_.inputs) {
            let clif_value = self.take(arg);
            self.set(input, clif_value);
        }

        let then_block = fb.create_block();
        let else_block = fb.create_block();
        let after_block = fb.create_block();

        let condition = self.take(condition);
        fb.ins().brif(condition, then_block, &[], else_block, &[]);
        fb.seal_block(then_block);
        fb.seal_block(else_block);

        fb.switch_to_block(then_block);
        for assignment in then.assignments {
            self.compile_assignment(assignment, fb);
        }
        for (value, &out) in std::iter::zip(&to, &then.outputs) {
            let v = self.take(out);
            self.set(
                value,
                fb.append_block_param(after_block, fb.func.dfg.value_type(v)),
            );
        }
        fb.ins().jump(
            after_block,
            &then
                .outputs
                .iter()
                .map(|&out| self.take(out))
                .collect::<Vec<_>>(),
        );

        fb.switch_to_block(else_block);
        for assignment in else_.assignments {
            self.compile_assignment(assignment, fb);
        }
        fb.ins().jump(
            after_block,
            &else_
                .outputs
                .iter()
                .map(|&out| self.take(out))
                .collect::<Vec<_>>(),
        );
        fb.seal_block(after_block);

        fb.switch_to_block(after_block);
    }

    fn compile_repeat(
        &mut self,
        to: ssa::ValueSequence,
        args: &[ssa::Value],
        body: ssa::Graph,
        fb: &mut FunctionBuilder,
    ) {
        let loop_block = fb.create_block();
        let after_block = fb.create_block();

        for (arg, &input) in std::iter::zip(args, &body.inputs) {
            let v = self.ssa_values[arg];
            self.set(
                input,
                fb.append_block_param(loop_block, fb.func.dfg.value_type(v)),
            );
        }

        fb.ins().jump(
            loop_block,
            &args.iter().map(|&arg| self.take(arg)).collect::<Vec<_>>(),
        );
        fb.switch_to_block(loop_block);
        for assignment in body.assignments {
            self.compile_assignment(assignment, fb);
        }
        let (&condition, outputs) = body.outputs.split_last().unwrap();
        for (value, out) in std::iter::zip(&to, outputs) {
            self.set(value, self.ssa_values[out]);
        }
        fb.ins().brif(
            self.take(condition),
            loop_block,
            &outputs
                .iter()
                .map(|&out| self.take(out))
                .collect::<Vec<_>>(),
            after_block,
            &[],
        );
        fb.seal_block(loop_block);
        fb.seal_block(after_block);

        fb.switch_to_block(after_block);
    }
}

fn extern_function_signatures(
    isa: &dyn TargetIsa,
) -> HashMap<&'static str, Signature> {
    let call_conv = isa.default_call_conv();

    HashMap::from([
        (
            "spkl_print_char",
            Signature {
                params: vec![AbiParam::new(I32)],
                returns: Vec::new(),
                call_conv,
            },
        ),
        (
            "spkl_print_i32",
            Signature {
                params: vec![AbiParam::new(I32)],
                returns: Vec::new(),
                call_conv,
            },
        ),
        (
            "spkl_println_i32",
            Signature {
                params: vec![AbiParam::new(I32)],
                returns: Vec::new(),
                call_conv,
            },
        ),
        (
            "spkl_print_f32",
            Signature {
                params: vec![AbiParam::new(F32)],
                returns: Vec::new(),
                call_conv,
            },
        ),
        (
            "spkl_println_f32",
            Signature {
                params: vec![AbiParam::new(F32)],
                returns: Vec::new(),
                call_conv,
            },
        ),
    ])
}

impl Type {
    fn to_clif(&self, isa: &dyn TargetIsa) -> Option<cranelift::prelude::Type> {
        Some(match self {
            Self::Bool => I8,
            Self::I32 => I32,
            Self::F32 => F32,
            Self::Type => return None,
            Self::Ptr(_) => isa.pointer_type(),
        })
    }
}

impl FunctionSignature {
    fn to_clif(&self, name: &str, isa: &dyn TargetIsa) -> Signature {
        let params = self
            .parameters
            .iter()
            .map(|typ| AbiParam::new(typ.to_clif(isa).unwrap()))
            .collect();
        let mut returns = self
            .returns
            .iter()
            .map(|typ| AbiParam::new(typ.to_clif(isa).unwrap()))
            .collect::<Vec<_>>();
        if name == "main" {
            returns.push(AbiParam::new(I32));
        }

        Signature {
            params,
            returns,
            call_conv: isa.default_call_conv(),
        }
    }
}
