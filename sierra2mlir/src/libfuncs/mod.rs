use std::{cell::RefCell, cmp::Ordering, rc::Rc};

use cairo_lang_sierra::program::{GenericArg, LibfuncDeclaration};
use color_eyre::Result;
use itertools::Itertools;
use melior_next::ir::{Block, BlockRef, Location, Region, Type, TypeLike, Value};
use tracing::debug;

use crate::{
    compiler::{Compiler, FunctionDef, SierraType, Storage},
    statements::create_fn_signature,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl<'ctx> Compiler<'ctx> {
    pub fn process_libfuncs(&'ctx self, storage: Rc<RefCell<Storage<'ctx>>>) -> Result<()> {
        for func_decl in &self.program.libfunc_declarations {
            let id = func_decl.id.id;
            let name = func_decl.long_id.generic_id.0.as_str();
            debug!(name, id, "processing libfunc decl");

            let parent_block = self.module.body();

            match name {
                // no-ops
                "revoke_ap_tracking" => continue,
                "disable_ap_tracking" => continue,
                "drop" => continue,
                "felt252_const" => {
                    self.create_libfunc_felt_const(func_decl, &mut storage.borrow_mut());
                }
                "felt252_add" => {
                    self.create_libfunc_felt_binary_op(
                        func_decl,
                        parent_block,
                        storage.clone(),
                        BinaryOp::Add,
                    )?;
                }
                "felt252_sub" => {
                    self.create_libfunc_felt_binary_op(
                        func_decl,
                        parent_block,
                        storage.clone(),
                        BinaryOp::Sub,
                    )?;
                }
                "felt252_mul" => {
                    self.create_libfunc_felt_binary_op(
                        func_decl,
                        parent_block,
                        storage.clone(),
                        BinaryOp::Mul,
                    )?;
                }
                "dup" => {
                    self.create_libfunc_dup(func_decl, parent_block, storage.clone())?;
                }
                "struct_construct" => {
                    self.create_libfunc_struct_construct(func_decl, parent_block, storage.clone())?;
                }
                "store_temp" | "rename" => {
                    self.create_libfunc_store_temp(func_decl, parent_block, storage.clone())?;
                }
                "u8_const" => {
                    self.create_libfunc_u8_const(func_decl, &mut storage.borrow_mut());
                }
                "u16_const" => {
                    self.create_libfunc_u16_const(func_decl, &mut storage.borrow_mut());
                }
                "u32_const" => {
                    self.create_libfunc_u32_const(func_decl, &mut storage.borrow_mut());
                }
                "u64_const" => {
                    self.create_libfunc_u64_const(func_decl, &mut storage.borrow_mut());
                }
                "u128_const" => {
                    self.create_libfunc_u128_const(func_decl, &mut storage.borrow_mut());
                }
                "upcast" => {
                    self.create_libfunc_upcast(func_decl, parent_block, &mut storage.borrow_mut())?;
                }
                _ => debug!(?func_decl, "unhandled libfunc"),
            }
        }

        debug!(types = ?RefCell::borrow(&*storage).types, "processed");
        Ok(())
    }

    pub fn create_libfunc_felt_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match &func_decl.long_id.generic_args[0] {
            GenericArg::Value(value) => value.to_string(),
            _ => unimplemented!("should always be value"),
        };

        storage.felt_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_ref().unwrap().as_str())
                .to_string(),
            arg,
        );
    }

    pub fn create_libfunc_struct_construct(
        &'ctx self,
        func_decl: &LibfuncDeclaration,
        parent_block: BlockRef<'ctx>,
        storage: Rc<RefCell<Storage<'ctx>>>,
    ) -> Result<()> {
        let id = Self::normalize_func_name(func_decl.id.debug_name.as_ref().unwrap().as_str())
            .to_string();
        let arg_type = match &func_decl.long_id.generic_args[0] {
            GenericArg::UserType(_) => todo!(),
            GenericArg::Type(type_id) => {
                let storage = RefCell::borrow(&*storage);
                let ty = storage
                    .types
                    .get(&type_id.id.to_string())
                    .cloned()
                    .expect("type to exist");

                ty
            }
            GenericArg::Value(_) => todo!(),
            GenericArg::UserFunc(_) => todo!(),
            GenericArg::Libfunc(_) => todo!(),
        };

        let args = arg_type
            .get_field_types()
            .expect("arg should be a struct type and have field types");
        let args_with_location = args
            .iter()
            .map(|x| (*x, Location::unknown(&self.context)))
            .collect_vec();

        let region = Region::new();

        let block = Block::new(&args_with_location);

        let struct_llvm_type = self.struct_type_string(&args);
        let mut struct_type_op = self.op_llvm_struct(&block, &args);

        for i in 0..block.argument_count() {
            let arg = block.argument(i)?;
            let struct_value = struct_type_op.result(0)?.into();
            struct_type_op =
                self.op_llvm_insertvalue(&block, i, struct_value, arg.into(), &struct_llvm_type)?;
        }

        let struct_value: Value = struct_type_op.result(0)?.into();
        self.op_return(&block, &[struct_value]);

        let return_type = Type::parse(&self.context, &struct_llvm_type).unwrap();
        let function_type = create_fn_signature(&args, &[return_type]);

        region.append_block(block);

        let func = self.op_func(&id, &function_type, vec![region], false, false)?;

        {
            let mut storage = storage.borrow_mut();
            storage.functions.insert(
                id,
                FunctionDef {
                    args: arg_type.get_field_sierra_types().unwrap().to_vec(),
                    return_types: vec![arg_type],
                },
            );
        }

        parent_block.append_operation(func);

        Ok(())
    }

    /// Returns the given value, needed so its handled nicely when processing statements
    /// and the variable id gets assigned to the returned value.
    pub fn create_libfunc_store_temp(
        &'ctx self,
        func_decl: &LibfuncDeclaration,
        parent_block: BlockRef<'ctx>,
        storage: Rc<RefCell<Storage<'ctx>>>,
    ) -> Result<()> {
        let id = Self::normalize_func_name(func_decl.id.debug_name.as_ref().unwrap().as_str())
            .to_string();

        let arg_type = match &func_decl.long_id.generic_args[0] {
            GenericArg::UserType(_) => todo!(),
            GenericArg::Type(type_id) => {
                let storage = RefCell::borrow(&*storage);
                let ty = storage
                    .types
                    .get(&type_id.id.to_string())
                    .expect("type to exist");

                ty.clone()
            }
            GenericArg::Value(_) => todo!(),
            GenericArg::UserFunc(_) => todo!(),
            GenericArg::Libfunc(_) => todo!(),
        };

        let region = Region::new();

        let args = &[arg_type.get_type()];
        let args_with_location = &[arg_type.get_type_location(&self.context)];

        let block = Block::new(args_with_location);

        let mut results: Vec<Value> = vec![];

        for i in 0..block.argument_count() {
            let arg = block.argument(i)?;
            results.push(arg.into());
        }

        self.op_return(&block, &results);

        region.append_block(block);

        let function_type = create_fn_signature(args, args);

        let func = self.op_func(&id, &function_type, vec![region], false, false)?;

        {
            let mut storage = storage.borrow_mut();
            storage.functions.insert(
                id,
                FunctionDef {
                    args: vec![arg_type.clone()],
                    return_types: vec![arg_type],
                },
            );
        }

        parent_block.append_operation(func);

        Ok(())
    }

    pub fn create_libfunc_dup(
        &'ctx self,
        func_decl: &LibfuncDeclaration,
        parent_block: BlockRef<'ctx>,
        storage: Rc<RefCell<Storage<'ctx>>>,
    ) -> Result<()> {
        let id = Self::normalize_func_name(func_decl.id.debug_name.as_ref().unwrap().as_str())
            .to_string();
        let arg_type = match &func_decl.long_id.generic_args[0] {
            GenericArg::UserType(_) => todo!(),
            GenericArg::Type(type_id) => {
                let storage = RefCell::borrow(&*storage);
                let ty = storage
                    .types
                    .get(&type_id.id.to_string())
                    .expect("type to exist");

                ty.clone()
            }
            GenericArg::Value(_) => todo!(),
            GenericArg::UserFunc(_) => todo!(),
            GenericArg::Libfunc(_) => todo!(),
        };

        let region = Region::new();

        let args = &[arg_type.get_type()];
        let args_with_location = &[arg_type.get_type_location(&self.context)];

        let block = Block::new(args_with_location);

        // Return the results, 2 times.
        let mut results: Vec<Value> = vec![];

        for i in 0..block.argument_count() {
            let arg = block.argument(i)?;
            results.push(arg.into());
        }

        // 2 times, duplicate.
        for i in 0..block.argument_count() {
            let arg = block.argument(i)?;
            results.push(arg.into());
        }

        self.op_return(&block, &results);

        region.append_block(block);

        let mut return_types = Vec::with_capacity(args.len() * 2);
        return_types.extend_from_slice(args);
        return_types.extend_from_slice(args);

        let function_type = create_fn_signature(args, &return_types);

        let func = self.op_func(&id, &function_type, vec![region], false, false)?;

        {
            let mut storage = storage.borrow_mut();
            storage.functions.insert(
                id,
                FunctionDef {
                    args: vec![arg_type.clone()],
                    return_types: vec![arg_type.clone(), arg_type],
                },
            );
        }

        parent_block.append_operation(func);

        Ok(())
    }

    pub fn create_libfunc_felt_binary_op(
        &'ctx self,
        func_decl: &LibfuncDeclaration,
        parent_block: BlockRef<'ctx>,
        storage: Rc<RefCell<Storage<'ctx>>>,
        binary_op: BinaryOp,
    ) -> Result<()> {
        let id = Self::normalize_func_name(func_decl.id.debug_name.as_ref().unwrap().as_str())
            .to_string();
        let sierra_felt_type = SierraType::Simple(self.felt_type());
        let felt_type = sierra_felt_type.get_type();
        let felt_type_location = sierra_felt_type.get_type_location(&self.context);
        dbg!(func_decl);

        let region = Region::new();
        let block = Block::new(&[felt_type_location, felt_type_location]);

        let lhs_arg = block.argument(0)?;
        let rhs_arg = block.argument(1)?;

        let lhs_ext = self.op_sext(&block, lhs_arg.into(), self.double_felt_type());
        let lhs = lhs_ext.result(0)?;

        let rhs_ext = self.op_sext(&block, rhs_arg.into(), self.double_felt_type());
        let rhs = rhs_ext.result(0)?;

        let res = match binary_op {
            BinaryOp::Add => self.op_add(&block, lhs.into(), rhs.into()),
            BinaryOp::Sub => self.op_sub(&block, lhs.into(), rhs.into()),
            BinaryOp::Mul => self.op_mul(&block, lhs.into(), rhs.into()),
            BinaryOp::Div => todo!(),
        };
        let res_result = res.result(0)?;

        let res = self.op_felt_modulo(&block, res_result.into())?;
        let res_result = res.result(0)?;

        self.op_return(&block, &[res_result.into()]);

        region.append_block(block);

        let func = self.op_func(
            &id,
            &format!("({felt_type}, {felt_type}) -> {felt_type}"),
            vec![region],
            false,
            false,
        )?;

        storage.borrow_mut().functions.insert(
            id,
            FunctionDef {
                args: vec![sierra_felt_type.clone(), sierra_felt_type.clone()],
                return_types: vec![sierra_felt_type],
            },
        );

        parent_block.append_operation(func);
        Ok(())
    }

    pub fn create_libfunc_u8_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match func_decl.long_id.generic_args.as_slice() {
            [GenericArg::Value(value)] => value.to_string(),
            _ => todo!(),
        };

        storage.u8_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).into_owned(),
            arg,
        );
    }

    pub fn create_libfunc_u16_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match func_decl.long_id.generic_args.as_slice() {
            [GenericArg::Value(value)] => value.to_string(),
            _ => todo!(),
        };

        storage.u16_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).into_owned(),
            arg,
        );
    }

    pub fn create_libfunc_u32_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match func_decl.long_id.generic_args.as_slice() {
            [GenericArg::Value(value)] => value.to_string(),
            _ => todo!(),
        };

        storage.u32_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).into_owned(),
            arg,
        );
    }

    pub fn create_libfunc_u64_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match func_decl.long_id.generic_args.as_slice() {
            [GenericArg::Value(value)] => value.to_string(),
            _ => todo!(),
        };

        storage.u64_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).into_owned(),
            arg,
        );
    }

    pub fn create_libfunc_u128_const(
        &self,
        func_decl: &LibfuncDeclaration,
        storage: &mut Storage<'ctx>,
    ) {
        let arg = match func_decl.long_id.generic_args.as_slice() {
            [GenericArg::Value(value)] => value.to_string(),
            _ => todo!(),
        };

        storage.u128_consts.insert(
            Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).into_owned(),
            arg,
        );
    }

    pub fn create_libfunc_upcast(
        &self,
        func_decl: &LibfuncDeclaration,
        parent_block: BlockRef<'ctx>,
        storage: &mut Storage<'ctx>,
    ) -> Result<()> {
        let id = Self::normalize_func_name(func_decl.id.debug_name.as_deref().unwrap()).to_string();

        let src_sierra_type = storage
            .types
            .get(&match &func_decl.long_id.generic_args[0] {
                GenericArg::Type(x) => x.id.to_string(),
                _ => todo!("invalid generic kind"),
            })
            .expect("type to exist");
        let dst_sierra_type = storage
            .types
            .get(&match &func_decl.long_id.generic_args[1] {
                GenericArg::Type(x) => x.id.to_string(),
                _ => todo!("invalid generic kind"),
            })
            .expect("type to exist");

        let src_type = src_sierra_type.get_type();
        let dst_type = dst_sierra_type.get_type();

        match src_type
            .get_width()
            .unwrap()
            .cmp(&dst_type.get_width().unwrap())
        {
            Ordering::Less => {
                let region = Region::new();
                let block = Block::new(&[(src_type, Location::unknown(&self.context))]);

                let op_ref = self.op_zext(&block, block.argument(0)?.into(), dst_type);

                self.op_return(&block, &[op_ref.result(0)?.into()]);
                region.append_block(block);

                let func = self.op_func(
                    &id,
                    &format!("({src_type}) -> {dst_type}"),
                    vec![region],
                    false,
                    false,
                )?;

                storage.functions.insert(
                    id,
                    FunctionDef {
                        args: vec![src_sierra_type.clone()],
                        return_types: vec![dst_sierra_type.clone()],
                    },
                );

                parent_block.append_operation(func);
            }
            Ordering::Equal => {}
            Ordering::Greater => todo!("invalid generics for libfunc `upcast`"),
        }

        Ok(())
    }
}
