use super::*;

#[test]
fn native_region_generated_constant_division_is_exact() {
    let mut module = JITModule::new(JITBuilder::new(default_libcall_names()).unwrap());
    let mut frontend = FunctionBuilderContext::new();
    let mut divisors: Vec<u32> = (1..=128).collect();
    divisors.extend([
        132,
        255,
        256,
        257,
        1023,
        1024,
        1025,
        16383,
        65535,
        65536,
        65537,
        TRACE_RETURN_CYCLES_MASK,
        1 << 31,
        u32::MAX - 1,
        u32::MAX,
    ]);
    for divisor in divisors {
        let mut signature = module.make_signature();
        signature.params.push(AbiParam::new(types::I32));
        signature.returns.push(AbiParam::new(types::I32));
        let id = module
            .declare_function(&format!("quotient_{divisor}"), Linkage::Local, &signature)
            .unwrap();
        let mut context = Context::new();
        context.func = Function::with_name_signature(UserFuncName::user(0, id.as_u32()), signature);
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
            let entry = builder.create_block();
            builder.switch_to_block(entry);
            builder.append_block_params_for_function_params(entry);
            let argument = builder.block_params(entry)[0];
            let result = constant_quotient(&mut builder, argument, divisor);
            builder.ins().return_(&[result]);
            builder.seal_all_blocks();
            builder.finalize();
        }
        module.define_function(id, &mut context).unwrap();
        module.clear_context(&mut context);
        module.finalize_definitions().unwrap();
        // SAFETY: the module owns this finalized function until every call below completes;
        // its signature matches the native ABI and the emitted function only does arithmetic.
        let divide = unsafe {
            transmute::<*const u8, unsafe extern "C" fn(u32) -> u32>(
                module.get_finalized_function(id),
            )
        };
        let mut values = vec![
            0,
            1,
            divisor - 1,
            divisor,
            divisor.saturating_add(1),
            TRACE_RETURN_CYCLES_MASK,
            i32::MAX as u32,
            1 << 31,
            u32::MAX - 1,
            u32::MAX,
        ];
        // Exact and adjacent multiples exercise quotient changes and reciprocal correction.
        for quotient in [2u64, 3, 7, 31, 255, 65535, u64::from(u32::MAX / divisor)] {
            let multiple = quotient * u64::from(divisor);
            for value in [multiple.saturating_sub(1), multiple, multiple + 1] {
                if let Ok(value) = u32::try_from(value) {
                    values.push(value);
                }
            }
        }
        let mut random = divisor ^ 0xa5c3_71d9;
        for _ in 0..4096 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            values.push(random);
        }
        for value in values {
            assert_eq!(
                unsafe { divide(value) },
                value / divisor,
                "{value}/{divisor}"
            );
        }
    }
}
