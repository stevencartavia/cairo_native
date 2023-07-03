//! # Enum-related libfuncs
//!
//! Check out [the enum type](crate::types::r#enum) for more information on enum layouts.

use super::{LibfuncBuilder, LibfuncHelper};
use crate::{
    error::{
        libfuncs::{Error, Result},
        CoreTypeBuilderError,
    },
    metadata::MetadataStorage,
    types::TypeBuilder,
};
use cairo_lang_sierra::{
    extensions::{
        enm::{EnumConcreteLibfunc, EnumInitConcreteLibfunc},
        lib_func::SignatureOnlyConcreteLibfunc,
        ConcreteLibfunc, GenericLibfunc, GenericType,
    },
    program_registry::ProgramRegistry,
};
use melior::{
    dialect::{
        arith, cf,
        llvm::{self, LoadStoreOptions},
    },
    ir::{
        attribute::{DenseI64ArrayAttribute, IntegerAttribute},
        operation::OperationBuilder,
        r#type::IntegerType,
        Block, Identifier, Location,
    },
    Context,
};

/// Select and call the correct libfunc builder function from the selector.
pub fn build<'ctx, 'this, TType, TLibfunc>(
    context: &'ctx Context,
    registry: &ProgramRegistry<TType, TLibfunc>,
    entry: &'this Block<'ctx>,
    location: Location<'ctx>,
    helper: &LibfuncHelper<'ctx, 'this>,
    metadata: &mut MetadataStorage,
    selector: &EnumConcreteLibfunc,
) -> Result<()>
where
    TType: GenericType,
    TLibfunc: GenericLibfunc,
    <TType as GenericType>::Concrete: TypeBuilder<TType, TLibfunc, Error = CoreTypeBuilderError>,
    <TLibfunc as GenericLibfunc>::Concrete: LibfuncBuilder<TType, TLibfunc, Error = Error>,
{
    match selector {
        EnumConcreteLibfunc::Init(info) => {
            build_init(context, registry, entry, location, helper, metadata, info)
        }
        EnumConcreteLibfunc::Match(info) => {
            build_match(context, registry, entry, location, helper, metadata, info)
        }
        EnumConcreteLibfunc::SnapshotMatch(_) => todo!(),
    }
}

/// Generate MLIR operations for the `enum_init` libfunc.
pub fn build_init<'ctx, 'this, TType, TLibfunc>(
    context: &'ctx Context,
    registry: &ProgramRegistry<TType, TLibfunc>,
    entry: &'this Block<'ctx>,
    location: Location<'ctx>,
    helper: &LibfuncHelper<'ctx, 'this>,
    metadata: &mut MetadataStorage,
    info: &EnumInitConcreteLibfunc,
) -> Result<()>
where
    TType: GenericType,
    TLibfunc: GenericLibfunc,
    <TType as GenericType>::Concrete: TypeBuilder<TType, TLibfunc, Error = CoreTypeBuilderError>,
    <TLibfunc as GenericLibfunc>::Concrete: LibfuncBuilder<TType, TLibfunc, Error = Error>,
{
    let (layout, (tag_ty, _), variant_tys) = crate::types::r#enum::get_type_for_variants(
        context,
        helper,
        registry,
        metadata,
        registry
            .get_type(&info.branch_signatures()[0].vars[0].ty)?
            .variants()
            .unwrap(),
    )?;

    let enum_ty = registry
        .get_type(&info.branch_signatures()[0].vars[0].ty)?
        .build(context, helper, registry, metadata)?;

    let op0 = entry.append_operation(arith::constant(
        context,
        IntegerAttribute::new(info.index.try_into()?, tag_ty).into(),
        location,
    ));

    let concrete_enum_ty =
        llvm::r#type::r#struct(context, &[tag_ty, variant_tys[info.index].0], false);

    let op1 = entry.append_operation(llvm::undef(concrete_enum_ty, location));
    let op2 = entry.append_operation(llvm::insert_value(
        context,
        op1.result(0)?.into(),
        DenseI64ArrayAttribute::new(context, &[0]),
        op0.result(0)?.into(),
        location,
    ));
    let op3 = entry.append_operation(llvm::insert_value(
        context,
        op2.result(0)?.into(),
        DenseI64ArrayAttribute::new(context, &[1]),
        entry.argument(0)?.into(),
        location,
    ));

    let op4 = helper.init_block().append_operation(arith::constant(
        context,
        IntegerAttribute::new(1, IntegerType::new(context, 64).into()).into(),
        location,
    ));
    let op5 = helper.init_block().append_operation(
        OperationBuilder::new("llvm.alloca", location)
            .add_attributes(&[(
                Identifier::new(context, "alignment"),
                IntegerAttribute::new(
                    layout.align().try_into()?,
                    IntegerType::new(context, 64).into(),
                )
                .into(),
            )])
            .add_operands(&[op4.result(0)?.into()])
            .add_results(&[llvm::r#type::pointer(enum_ty, 0)])
            .build(),
    );

    let op6 = entry.append_operation(
        OperationBuilder::new("llvm.bitcast", location)
            .add_operands(&[op5.result(0)?.into()])
            .add_results(&[llvm::r#type::pointer(concrete_enum_ty, 0)])
            .build(),
    );
    entry.append_operation(llvm::store(
        context,
        op3.result(0)?.into(),
        op6.result(0)?.into(),
        location,
        LoadStoreOptions::default().align(Some(IntegerAttribute::new(
            layout.align().try_into()?,
            IntegerType::new(context, 64).into(),
        ))),
    ));

    let op7 = entry.append_operation(llvm::load(
        context,
        op5.result(0)?.into(),
        enum_ty,
        location,
        LoadStoreOptions::default().align(Some(IntegerAttribute::new(
            layout.align().try_into()?,
            IntegerType::new(context, 64).into(),
        ))),
    ));

    entry.append_operation(helper.br(0, &[op7.result(0)?.into()], location));

    Ok(())
}

/// Generate MLIR operations for the `enum_match` libfunc.
pub fn build_match<'ctx, 'this, TType, TLibfunc>(
    context: &'ctx Context,
    registry: &ProgramRegistry<TType, TLibfunc>,
    entry: &'this Block<'ctx>,
    location: Location<'ctx>,
    helper: &LibfuncHelper<'ctx, 'this>,
    metadata: &mut MetadataStorage,
    info: &SignatureOnlyConcreteLibfunc,
) -> Result<()>
where
    TType: GenericType,
    TLibfunc: GenericLibfunc,
    <TType as GenericType>::Concrete: TypeBuilder<TType, TLibfunc, Error = CoreTypeBuilderError>,
    <TLibfunc as GenericLibfunc>::Concrete: LibfuncBuilder<TType, TLibfunc, Error = Error>,
{
    let (layout, (tag_ty, _), variant_tys) = crate::types::r#enum::get_type_for_variants(
        context,
        helper,
        registry,
        metadata,
        registry
            .get_type(&info.param_signatures()[0].ty)?
            .variants()
            .unwrap(),
    )?;

    let enum_ty = registry
        .get_type(&info.param_signatures()[0].ty)?
        .build(context, helper, registry, metadata)?;

    let op0 = helper.init_block().append_operation(arith::constant(
        context,
        IntegerAttribute::new(1, IntegerType::new(context, 64).into()).into(),
        location,
    ));
    let op1 = helper.init_block().append_operation(
        OperationBuilder::new("llvm.alloca", location)
            .add_attributes(&[(
                Identifier::new(context, "alignment"),
                IntegerAttribute::new(
                    layout.align().try_into()?,
                    IntegerType::new(context, 64).into(),
                )
                .into(),
            )])
            .add_operands(&[op0.result(0)?.into()])
            .add_results(&[llvm::r#type::pointer(enum_ty, 0)])
            .build(),
    );
    entry.append_operation(llvm::store(
        context,
        entry.argument(0)?.into(),
        op1.result(0)?.into(),
        location,
        LoadStoreOptions::default().align(Some(IntegerAttribute::new(
            layout.align().try_into()?,
            IntegerType::new(context, 64).into(),
        ))),
    ));

    let default_block = helper.append_block(Block::new(&[]));
    let variant_blocks = variant_tys
        .iter()
        .map(|_| helper.append_block(Block::new(&[])))
        .collect::<Vec<_>>();

    let op2 = entry.append_operation(llvm::extract_value(
        context,
        entry.argument(0)?.into(),
        DenseI64ArrayAttribute::new(context, &[0]),
        tag_ty,
        location,
    ));
    entry.append_operation(cf::switch(
        context,
        &(0..variant_tys.len())
            .map(i64::try_from)
            .try_collect::<Vec<_>>()?,
        op2.result(0)?.into(),
        tag_ty,
        (default_block, &[]),
        &variant_blocks
            .iter()
            .copied()
            .map(|block| (block, [].as_slice()))
            .collect::<Vec<_>>(),
        location,
    )?);

    {
        let op3 = default_block.append_operation(arith::constant(
            context,
            IntegerAttribute::new(0, IntegerType::new(context, 1).into()).into(),
            location,
        ));

        default_block.append_operation(cf::assert(
            context,
            op3.result(0)?.into(),
            "Invalid enum tag.",
            location,
        ));
        default_block.append_operation(OperationBuilder::new("llvm.unreachable", location).build());
    }

    for (i, (block, (payload_ty, _))) in variant_blocks.into_iter().zip(variant_tys).enumerate() {
        let concrete_enum_ty = llvm::r#type::r#struct(context, &[tag_ty, payload_ty], false);

        let op3 = block.append_operation(
            OperationBuilder::new("llvm.bitcast", location)
                .add_operands(&[op1.result(0)?.into()])
                .add_results(&[llvm::r#type::pointer(concrete_enum_ty, 0)])
                .build(),
        );
        let op4 = block.append_operation(llvm::load(
            context,
            op3.result(0)?.into(),
            concrete_enum_ty,
            location,
            LoadStoreOptions::default(),
        ));
        let op5 = block.append_operation(llvm::extract_value(
            context,
            op4.result(0)?.into(),
            DenseI64ArrayAttribute::new(context, &[1]),
            payload_ty,
            location,
        ));

        block.append_operation(helper.br(i, &[op5.result(0)?.into()], location));
    }

    Ok(())
}