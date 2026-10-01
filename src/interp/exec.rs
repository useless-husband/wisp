//! The interpreter loop.
//!
//! Wasm-to-wasm calls between interpreted functions do not recurse on the native stack: the
//! callee frame is laid over the caller's argument slots (arguments are already in place and
//! results land where the caller expects them) and a small frame record is pushed.

use super::bytecode::*;
use crate::error::{Trap, TrapCode};
use crate::num;
use crate::runtime::store::StoreInner;
use crate::runtime::vm::*;
use std::ptr;

struct Frame {
    pc: *const Instr,
    code: *const Instr,
    fp: *mut u64,
    vmctx: *mut VmCtx,
    func: *const InterpFunc,
}

#[inline(always)]
unsafe fn interp_func(fr: *const VmFuncRef) -> *const InterpFunc {
    unsafe {
        let m = (*(*fr).vmctx).interp as *const InterpModule;
        (&(*m).funcs)
            .get_unchecked((*fr).def_index as usize)
            .as_ref()
            .unwrap_unchecked() as *const InterpFunc
    }
}

/// Run interpreted function `fr` whose frame starts at `fp` (arguments already in place).
/// On success the results are in `fp[0..nresults]`.
///
/// # Safety
/// `fr` must be an interpreted function of a live instance in `store`, and `fp` must point
/// into the store's value stack with room for the function's frame.
pub(crate) unsafe fn execute(
    store: *mut StoreInner,
    fr: *const VmFuncRef,
    fp: *mut u64,
) -> Result<(), Trap> {
    unsafe {
        let st = &mut *store;
        let stack_base = st.istack.as_mut_ptr();
        let stack_end = stack_base.add(st.istack.len());
        let max_depth = st.max_depth;
        let rt: *mut VmRuntime = &mut *st.runtime;

        let mut func = interp_func(fr);
        let mut vmctx = (*fr).vmctx;
        let mut code = (*func).code.as_ptr();
        let mut pc = code;
        let mut fp = fp;
        if fp.add((*func).frame_size as usize) > stack_end || st.depth >= max_depth {
            return Err(TrapCode::StackExhausted.into());
        }
        ptr::write_bytes(
            fp.add((*func).nparams as usize),
            0,
            (*func).nlocals as usize,
        );
        let entry_depth = st.depth;
        st.depth += 1;
        let mut frames: Vec<Frame> = Vec::new();

        let mut mbase: *mut u8;
        let mut msize: u64;
        macro_rules! reload {
            () => {{
                let m = (*vmctx).memory;
                if !m.is_null() {
                    mbase = (*m).base;
                    msize = (*m).size;
                } else {
                    mbase = ptr::null_mut();
                    msize = 0;
                }
            }};
        }
        reload!();

        macro_rules! trap {
            ($code:expr) => {{
                (*store).depth = entry_depth;
                return Err(Trap::from($code));
            }};
        }
        macro_rules! r {
            ($s:expr) => {
                *fp.add($s as usize)
            };
        }
        macro_rules! w {
            ($s:expr, $v:expr) => {{
                let v = $v;
                *fp.add($s as usize) = v;
            }};
        }
        macro_rules! un {
            ($d:expr, $a:expr, $f:path) => {
                w!($d, $f(r!($a)))
            };
        }
        macro_rules! bin {
            ($d:expr, $a:expr, $b:expr, $f:path) => {
                w!($d, $f(r!($a), r!($b)))
            };
        }
        macro_rules! un_t {
            ($d:expr, $a:expr, $f:path) => {
                match $f(r!($a)) {
                    Ok(v) => w!($d, v),
                    Err(t) => trap!(t),
                }
            };
        }
        macro_rules! bin_t {
            ($d:expr, $a:expr, $b:expr, $f:path) => {
                match $f(r!($a), r!($b)) {
                    Ok(v) => w!($d, v),
                    Err(t) => trap!(t),
                }
            };
        }
        macro_rules! imm32 {
            ($d:expr, $a:expr, $imm:expr, $f:path) => {
                w!($d, $f(r!($a), $imm as u64))
            };
        }
        macro_rules! imm64 {
            ($d:expr, $a:expr, $imm:expr, $f:path) => {
                w!($d, $f(r!($a), $imm as i32 as i64 as u64))
            };
        }
        macro_rules! ea {
            ($a:expr, $off:expr, $n:expr) => {{
                let ea = (r!($a) as u32 as u64) + $off as u64;
                if ea + $n > msize {
                    trap!(TrapCode::MemoryOutOfBounds);
                }
                mbase.add(ea as usize)
            }};
        }
        macro_rules! load {
            ($d:expr, $a:expr, $off:expr, $t:ty, $conv:expr) => {{
                let p = ea!($a, $off, std::mem::size_of::<$t>() as u64);
                let v = <$t>::from_le(ptr::read_unaligned(p as *const $t));
                w!($d, $conv(v))
            }};
        }
        macro_rules! store {
            ($a:expr, $v:expr, $off:expr, $t:ty) => {{
                let p = ea!($a, $off, std::mem::size_of::<$t>() as u64);
                ptr::write_unaligned(p as *mut $t, (r!($v) as $t).to_le());
            }};
        }
        macro_rules! brc {
            ($c:expr, $a:expr, $b:expr, $t:expr) => {
                if $c.eval(r!($a) as u32, $b) {
                    pc = code.add($t as usize);
                }
            };
        }

        // Call through function reference `cfr` with the callee frame at `base`.
        macro_rules! call {
            ($cfr:expr, $base:expr) => {{
                let cfr: *const VmFuncRef = $cfr;
                let nfp = fp.add($base as usize);
                if (*cfr).kind == KIND_INTERP {
                    let callee = interp_func(cfr);
                    if nfp.add((*callee).frame_size as usize) > stack_end
                        || (*store).depth >= max_depth
                    {
                        trap!(TrapCode::StackExhausted);
                    }
                    ptr::write_bytes(
                        nfp.add((*callee).nparams as usize),
                        0,
                        (*callee).nlocals as usize,
                    );
                    frames.push(Frame {
                        pc,
                        code,
                        fp,
                        vmctx,
                        func,
                    });
                    (*store).depth += 1;
                    func = callee;
                    vmctx = (*cfr).vmctx;
                    fp = nfp;
                    code = (*func).code.as_ptr();
                    pc = code;
                    reload!();
                } else {
                    let top = fp.offset_from(stack_base) as usize + (*func).frame_size as usize;
                    if let Err(t) = (*store).call_out(cfr, nfp, vmctx, top) {
                        (*store).depth = entry_depth;
                        return Err(t);
                    }
                    reload!();
                }
            }};
        }

        loop {
            let ins = *pc;
            pc = pc.add(1);
            match ins {
                Instr::I32Eqz { d, a } => un!(d, a, num::i32_eqz),
                Instr::I64Eqz { d, a } => un!(d, a, num::i64_eqz),
                Instr::I32Clz { d, a } => un!(d, a, num::i32_clz),
                Instr::I32Ctz { d, a } => un!(d, a, num::i32_ctz),
                Instr::I32Popcnt { d, a } => un!(d, a, num::i32_popcnt),
                Instr::I64Clz { d, a } => un!(d, a, num::i64_clz),
                Instr::I64Ctz { d, a } => un!(d, a, num::i64_ctz),
                Instr::I64Popcnt { d, a } => un!(d, a, num::i64_popcnt),
                Instr::F32Abs { d, a } => un!(d, a, num::f32_abs),
                Instr::F32Neg { d, a } => un!(d, a, num::f32_neg),
                Instr::F32Ceil { d, a } => un!(d, a, num::f32_ceil),
                Instr::F32Floor { d, a } => un!(d, a, num::f32_floor),
                Instr::F32Trunc { d, a } => un!(d, a, num::f32_trunc),
                Instr::F32Nearest { d, a } => un!(d, a, num::f32_nearest),
                Instr::F32Sqrt { d, a } => un!(d, a, num::f32_sqrt),
                Instr::F64Abs { d, a } => un!(d, a, num::f64_abs),
                Instr::F64Neg { d, a } => un!(d, a, num::f64_neg),
                Instr::F64Ceil { d, a } => un!(d, a, num::f64_ceil),
                Instr::F64Floor { d, a } => un!(d, a, num::f64_floor),
                Instr::F64Trunc { d, a } => un!(d, a, num::f64_trunc),
                Instr::F64Nearest { d, a } => un!(d, a, num::f64_nearest),
                Instr::F64Sqrt { d, a } => un!(d, a, num::f64_sqrt),
                Instr::I32WrapI64 { d, a } => un!(d, a, num::i32_wrap_i64),
                Instr::I32TruncF32S { d, a } => un_t!(d, a, num::i32_trunc_f32_s),
                Instr::I32TruncF32U { d, a } => un_t!(d, a, num::i32_trunc_f32_u),
                Instr::I32TruncF64S { d, a } => un_t!(d, a, num::i32_trunc_f64_s),
                Instr::I32TruncF64U { d, a } => un_t!(d, a, num::i32_trunc_f64_u),
                Instr::I64ExtendI32S { d, a } => un!(d, a, num::i64_extend_i32_s),
                Instr::I64ExtendI32U { d, a } => un!(d, a, num::i64_extend_i32_u),
                Instr::I64TruncF32S { d, a } => un_t!(d, a, num::i64_trunc_f32_s),
                Instr::I64TruncF32U { d, a } => un_t!(d, a, num::i64_trunc_f32_u),
                Instr::I64TruncF64S { d, a } => un_t!(d, a, num::i64_trunc_f64_s),
                Instr::I64TruncF64U { d, a } => un_t!(d, a, num::i64_trunc_f64_u),
                Instr::F32ConvertI32S { d, a } => un!(d, a, num::f32_convert_i32_s),
                Instr::F32ConvertI32U { d, a } => un!(d, a, num::f32_convert_i32_u),
                Instr::F32ConvertI64S { d, a } => un!(d, a, num::f32_convert_i64_s),
                Instr::F32ConvertI64U { d, a } => un!(d, a, num::f32_convert_i64_u),
                Instr::F32DemoteF64 { d, a } => un!(d, a, num::f32_demote_f64),
                Instr::F64ConvertI32S { d, a } => un!(d, a, num::f64_convert_i32_s),
                Instr::F64ConvertI32U { d, a } => un!(d, a, num::f64_convert_i32_u),
                Instr::F64ConvertI64S { d, a } => un!(d, a, num::f64_convert_i64_s),
                Instr::F64ConvertI64U { d, a } => un!(d, a, num::f64_convert_i64_u),
                Instr::F64PromoteF32 { d, a } => un!(d, a, num::f64_promote_f32),
                Instr::I32ReinterpretF32 { d, a } => un!(d, a, num::reinterpret32),
                Instr::I64ReinterpretF64 { d, a } => un!(d, a, num::reinterpret64),
                Instr::F32ReinterpretI32 { d, a } => un!(d, a, num::reinterpret32),
                Instr::F64ReinterpretI64 { d, a } => un!(d, a, num::reinterpret64),
                Instr::I32Extend8S { d, a } => un!(d, a, num::i32_extend8_s),
                Instr::I32Extend16S { d, a } => un!(d, a, num::i32_extend16_s),
                Instr::I64Extend8S { d, a } => un!(d, a, num::i64_extend8_s),
                Instr::I64Extend16S { d, a } => un!(d, a, num::i64_extend16_s),
                Instr::I64Extend32S { d, a } => un!(d, a, num::i64_extend32_s),
                Instr::I32TruncSatF32S { d, a } => un!(d, a, num::i32_trunc_sat_f32_s),
                Instr::I32TruncSatF32U { d, a } => un!(d, a, num::i32_trunc_sat_f32_u),
                Instr::I32TruncSatF64S { d, a } => un!(d, a, num::i32_trunc_sat_f64_s),
                Instr::I32TruncSatF64U { d, a } => un!(d, a, num::i32_trunc_sat_f64_u),
                Instr::I64TruncSatF32S { d, a } => un!(d, a, num::i64_trunc_sat_f32_s),
                Instr::I64TruncSatF32U { d, a } => un!(d, a, num::i64_trunc_sat_f32_u),
                Instr::I64TruncSatF64S { d, a } => un!(d, a, num::i64_trunc_sat_f64_s),
                Instr::I64TruncSatF64U { d, a } => un!(d, a, num::i64_trunc_sat_f64_u),

                Instr::I32Eq { d, a, b } => bin!(d, a, b, num::i32_eq),
                Instr::I32Ne { d, a, b } => bin!(d, a, b, num::i32_ne),
                Instr::I32LtS { d, a, b } => bin!(d, a, b, num::i32_lt_s),
                Instr::I32LtU { d, a, b } => bin!(d, a, b, num::i32_lt_u),
                Instr::I32GtS { d, a, b } => bin!(d, a, b, num::i32_gt_s),
                Instr::I32GtU { d, a, b } => bin!(d, a, b, num::i32_gt_u),
                Instr::I32LeS { d, a, b } => bin!(d, a, b, num::i32_le_s),
                Instr::I32LeU { d, a, b } => bin!(d, a, b, num::i32_le_u),
                Instr::I32GeS { d, a, b } => bin!(d, a, b, num::i32_ge_s),
                Instr::I32GeU { d, a, b } => bin!(d, a, b, num::i32_ge_u),
                Instr::I64Eq { d, a, b } => bin!(d, a, b, num::i64_eq),
                Instr::I64Ne { d, a, b } => bin!(d, a, b, num::i64_ne),
                Instr::I64LtS { d, a, b } => bin!(d, a, b, num::i64_lt_s),
                Instr::I64LtU { d, a, b } => bin!(d, a, b, num::i64_lt_u),
                Instr::I64GtS { d, a, b } => bin!(d, a, b, num::i64_gt_s),
                Instr::I64GtU { d, a, b } => bin!(d, a, b, num::i64_gt_u),
                Instr::I64LeS { d, a, b } => bin!(d, a, b, num::i64_le_s),
                Instr::I64LeU { d, a, b } => bin!(d, a, b, num::i64_le_u),
                Instr::I64GeS { d, a, b } => bin!(d, a, b, num::i64_ge_s),
                Instr::I64GeU { d, a, b } => bin!(d, a, b, num::i64_ge_u),
                Instr::F32Eq { d, a, b } => bin!(d, a, b, num::f32_eq),
                Instr::F32Ne { d, a, b } => bin!(d, a, b, num::f32_ne),
                Instr::F32Lt { d, a, b } => bin!(d, a, b, num::f32_lt),
                Instr::F32Gt { d, a, b } => bin!(d, a, b, num::f32_gt),
                Instr::F32Le { d, a, b } => bin!(d, a, b, num::f32_le),
                Instr::F32Ge { d, a, b } => bin!(d, a, b, num::f32_ge),
                Instr::F64Eq { d, a, b } => bin!(d, a, b, num::f64_eq),
                Instr::F64Ne { d, a, b } => bin!(d, a, b, num::f64_ne),
                Instr::F64Lt { d, a, b } => bin!(d, a, b, num::f64_lt),
                Instr::F64Gt { d, a, b } => bin!(d, a, b, num::f64_gt),
                Instr::F64Le { d, a, b } => bin!(d, a, b, num::f64_le),
                Instr::F64Ge { d, a, b } => bin!(d, a, b, num::f64_ge),
                Instr::I32Add { d, a, b } => bin!(d, a, b, num::i32_add),
                Instr::I32Sub { d, a, b } => bin!(d, a, b, num::i32_sub),
                Instr::I32Mul { d, a, b } => bin!(d, a, b, num::i32_mul),
                Instr::I32DivS { d, a, b } => bin_t!(d, a, b, num::i32_div_s),
                Instr::I32DivU { d, a, b } => bin_t!(d, a, b, num::i32_div_u),
                Instr::I32RemS { d, a, b } => bin_t!(d, a, b, num::i32_rem_s),
                Instr::I32RemU { d, a, b } => bin_t!(d, a, b, num::i32_rem_u),
                Instr::I32And { d, a, b } => bin!(d, a, b, num::i32_and),
                Instr::I32Or { d, a, b } => bin!(d, a, b, num::i32_or),
                Instr::I32Xor { d, a, b } => bin!(d, a, b, num::i32_xor),
                Instr::I32Shl { d, a, b } => bin!(d, a, b, num::i32_shl),
                Instr::I32ShrS { d, a, b } => bin!(d, a, b, num::i32_shr_s),
                Instr::I32ShrU { d, a, b } => bin!(d, a, b, num::i32_shr_u),
                Instr::I32Rotl { d, a, b } => bin!(d, a, b, num::i32_rotl),
                Instr::I32Rotr { d, a, b } => bin!(d, a, b, num::i32_rotr),
                Instr::I64Add { d, a, b } => bin!(d, a, b, num::i64_add),
                Instr::I64Sub { d, a, b } => bin!(d, a, b, num::i64_sub),
                Instr::I64Mul { d, a, b } => bin!(d, a, b, num::i64_mul),
                Instr::I64DivS { d, a, b } => bin_t!(d, a, b, num::i64_div_s),
                Instr::I64DivU { d, a, b } => bin_t!(d, a, b, num::i64_div_u),
                Instr::I64RemS { d, a, b } => bin_t!(d, a, b, num::i64_rem_s),
                Instr::I64RemU { d, a, b } => bin_t!(d, a, b, num::i64_rem_u),
                Instr::I64And { d, a, b } => bin!(d, a, b, num::i64_and),
                Instr::I64Or { d, a, b } => bin!(d, a, b, num::i64_or),
                Instr::I64Xor { d, a, b } => bin!(d, a, b, num::i64_xor),
                Instr::I64Shl { d, a, b } => bin!(d, a, b, num::i64_shl),
                Instr::I64ShrS { d, a, b } => bin!(d, a, b, num::i64_shr_s),
                Instr::I64ShrU { d, a, b } => bin!(d, a, b, num::i64_shr_u),
                Instr::I64Rotl { d, a, b } => bin!(d, a, b, num::i64_rotl),
                Instr::I64Rotr { d, a, b } => bin!(d, a, b, num::i64_rotr),
                Instr::F32Add { d, a, b } => bin!(d, a, b, num::f32_add),
                Instr::F32Sub { d, a, b } => bin!(d, a, b, num::f32_sub),
                Instr::F32Mul { d, a, b } => bin!(d, a, b, num::f32_mul),
                Instr::F32Div { d, a, b } => bin!(d, a, b, num::f32_div),
                Instr::F32Min { d, a, b } => bin!(d, a, b, num::f32_min),
                Instr::F32Max { d, a, b } => bin!(d, a, b, num::f32_max),
                Instr::F32Copysign { d, a, b } => bin!(d, a, b, num::f32_copysign),
                Instr::F64Add { d, a, b } => bin!(d, a, b, num::f64_add),
                Instr::F64Sub { d, a, b } => bin!(d, a, b, num::f64_sub),
                Instr::F64Mul { d, a, b } => bin!(d, a, b, num::f64_mul),
                Instr::F64Div { d, a, b } => bin!(d, a, b, num::f64_div),
                Instr::F64Min { d, a, b } => bin!(d, a, b, num::f64_min),
                Instr::F64Max { d, a, b } => bin!(d, a, b, num::f64_max),
                Instr::F64Copysign { d, a, b } => bin!(d, a, b, num::f64_copysign),

                Instr::I32AddImm { d, a, imm } => imm32!(d, a, imm, num::i32_add),
                Instr::I32SubImm { d, a, imm } => imm32!(d, a, imm, num::i32_sub),
                Instr::I32MulImm { d, a, imm } => imm32!(d, a, imm, num::i32_mul),
                Instr::I32AndImm { d, a, imm } => imm32!(d, a, imm, num::i32_and),
                Instr::I32OrImm { d, a, imm } => imm32!(d, a, imm, num::i32_or),
                Instr::I32XorImm { d, a, imm } => imm32!(d, a, imm, num::i32_xor),
                Instr::I32ShlImm { d, a, imm } => imm32!(d, a, imm, num::i32_shl),
                Instr::I32ShrSImm { d, a, imm } => imm32!(d, a, imm, num::i32_shr_s),
                Instr::I32ShrUImm { d, a, imm } => imm32!(d, a, imm, num::i32_shr_u),
                Instr::I32EqImm { d, a, imm } => imm32!(d, a, imm, num::i32_eq),
                Instr::I32NeImm { d, a, imm } => imm32!(d, a, imm, num::i32_ne),
                Instr::I32LtSImm { d, a, imm } => imm32!(d, a, imm, num::i32_lt_s),
                Instr::I32LtUImm { d, a, imm } => imm32!(d, a, imm, num::i32_lt_u),
                Instr::I32GtSImm { d, a, imm } => imm32!(d, a, imm, num::i32_gt_s),
                Instr::I32GtUImm { d, a, imm } => imm32!(d, a, imm, num::i32_gt_u),
                Instr::I32LeSImm { d, a, imm } => imm32!(d, a, imm, num::i32_le_s),
                Instr::I32LeUImm { d, a, imm } => imm32!(d, a, imm, num::i32_le_u),
                Instr::I32GeSImm { d, a, imm } => imm32!(d, a, imm, num::i32_ge_s),
                Instr::I32GeUImm { d, a, imm } => imm32!(d, a, imm, num::i32_ge_u),
                Instr::I64AddImm { d, a, imm } => imm64!(d, a, imm, num::i64_add),
                Instr::I64SubImm { d, a, imm } => imm64!(d, a, imm, num::i64_sub),
                Instr::I64MulImm { d, a, imm } => imm64!(d, a, imm, num::i64_mul),
                Instr::I64AndImm { d, a, imm } => imm64!(d, a, imm, num::i64_and),
                Instr::I64OrImm { d, a, imm } => imm64!(d, a, imm, num::i64_or),
                Instr::I64XorImm { d, a, imm } => imm64!(d, a, imm, num::i64_xor),
                Instr::I64ShlImm { d, a, imm } => imm64!(d, a, imm, num::i64_shl),
                Instr::I64ShrSImm { d, a, imm } => imm64!(d, a, imm, num::i64_shr_s),
                Instr::I64ShrUImm { d, a, imm } => imm64!(d, a, imm, num::i64_shr_u),
                Instr::I64EqImm { d, a, imm } => imm64!(d, a, imm, num::i64_eq),
                Instr::I64NeImm { d, a, imm } => imm64!(d, a, imm, num::i64_ne),
                Instr::I64LtSImm { d, a, imm } => imm64!(d, a, imm, num::i64_lt_s),
                Instr::I64LtUImm { d, a, imm } => imm64!(d, a, imm, num::i64_lt_u),
                Instr::I64GtSImm { d, a, imm } => imm64!(d, a, imm, num::i64_gt_s),
                Instr::I64GtUImm { d, a, imm } => imm64!(d, a, imm, num::i64_gt_u),
                Instr::I64LeSImm { d, a, imm } => imm64!(d, a, imm, num::i64_le_s),
                Instr::I64LeUImm { d, a, imm } => imm64!(d, a, imm, num::i64_le_u),
                Instr::I64GeSImm { d, a, imm } => imm64!(d, a, imm, num::i64_ge_s),
                Instr::I64GeUImm { d, a, imm } => imm64!(d, a, imm, num::i64_ge_u),

                Instr::BrEq { a, b, t } => brc!(Cmp::Eq, a, r!(b) as u32, t),
                Instr::BrNe { a, b, t } => brc!(Cmp::Ne, a, r!(b) as u32, t),
                Instr::BrLtS { a, b, t } => brc!(Cmp::LtS, a, r!(b) as u32, t),
                Instr::BrLtU { a, b, t } => brc!(Cmp::LtU, a, r!(b) as u32, t),
                Instr::BrGtS { a, b, t } => brc!(Cmp::GtS, a, r!(b) as u32, t),
                Instr::BrGtU { a, b, t } => brc!(Cmp::GtU, a, r!(b) as u32, t),
                Instr::BrLeS { a, b, t } => brc!(Cmp::LeS, a, r!(b) as u32, t),
                Instr::BrLeU { a, b, t } => brc!(Cmp::LeU, a, r!(b) as u32, t),
                Instr::BrGeS { a, b, t } => brc!(Cmp::GeS, a, r!(b) as u32, t),
                Instr::BrGeU { a, b, t } => brc!(Cmp::GeU, a, r!(b) as u32, t),
                Instr::BrEqImm { a, imm, t } => brc!(Cmp::Eq, a, imm, t),
                Instr::BrNeImm { a, imm, t } => brc!(Cmp::Ne, a, imm, t),
                Instr::BrLtSImm { a, imm, t } => brc!(Cmp::LtS, a, imm, t),
                Instr::BrLtUImm { a, imm, t } => brc!(Cmp::LtU, a, imm, t),
                Instr::BrGtSImm { a, imm, t } => brc!(Cmp::GtS, a, imm, t),
                Instr::BrGtUImm { a, imm, t } => brc!(Cmp::GtU, a, imm, t),
                Instr::BrLeSImm { a, imm, t } => brc!(Cmp::LeS, a, imm, t),
                Instr::BrLeUImm { a, imm, t } => brc!(Cmp::LeU, a, imm, t),
                Instr::BrGeSImm { a, imm, t } => brc!(Cmp::GeS, a, imm, t),
                Instr::BrGeUImm { a, imm, t } => brc!(Cmp::GeU, a, imm, t),

                Instr::LoadI32 { d, a, off } => load!(d, a, off, u32, |v: u32| v as u64),
                Instr::LoadI64 { d, a, off } => load!(d, a, off, u64, |v: u64| v),
                Instr::LoadI32S8 { d, a, off } => {
                    load!(d, a, off, u8, |v: u8| v as i8 as i32 as u32 as u64)
                }
                Instr::LoadI32U8 { d, a, off } => load!(d, a, off, u8, |v: u8| v as u64),
                Instr::LoadI32S16 { d, a, off } => {
                    load!(d, a, off, u16, |v: u16| v as i16 as i32 as u32 as u64)
                }
                Instr::LoadI32U16 { d, a, off } => load!(d, a, off, u16, |v: u16| v as u64),
                Instr::LoadI64S8 { d, a, off } => {
                    load!(d, a, off, u8, |v: u8| v as i8 as i64 as u64)
                }
                Instr::LoadI64U8 { d, a, off } => load!(d, a, off, u8, |v: u8| v as u64),
                Instr::LoadI64S16 { d, a, off } => {
                    load!(d, a, off, u16, |v: u16| v as i16 as i64 as u64)
                }
                Instr::LoadI64U16 { d, a, off } => load!(d, a, off, u16, |v: u16| v as u64),
                Instr::LoadI64S32 { d, a, off } => {
                    load!(d, a, off, u32, |v: u32| v as i32 as i64 as u64)
                }
                Instr::LoadI64U32 { d, a, off } => load!(d, a, off, u32, |v: u32| v as u64),
                Instr::Store8 { a, v, off } => store!(a, v, off, u8),
                Instr::Store16 { a, v, off } => store!(a, v, off, u16),
                Instr::Store32 { a, v, off } => store!(a, v, off, u32),
                Instr::Store64 { a, v, off } => store!(a, v, off, u64),

                Instr::Trap { code: c } => {
                    trap!(TrapCode::from_raw(c).unwrap_or(TrapCode::Unreachable))
                }
                Instr::Br { t } => pc = code.add(t as usize),
                Instr::BrIfNez { c, t } => {
                    if r!(c) as u32 != 0 {
                        pc = code.add(t as usize);
                    }
                }
                Instr::BrIfEqz { c, t } => {
                    if r!(c) as u32 == 0 {
                        pc = code.add(t as usize);
                    }
                }
                Instr::BrTable { idx, len } => {
                    let i = r!(idx) as u32;
                    pc = pc.add(if i < len { i } else { len } as usize);
                }
                Instr::Return => match frames.pop() {
                    None => {
                        (*store).depth = entry_depth;
                        return Ok(());
                    }
                    Some(f) => {
                        pc = f.pc;
                        code = f.code;
                        fp = f.fp;
                        vmctx = f.vmctx;
                        func = f.func;
                        (*store).depth -= 1;
                        reload!();
                    }
                },
                Instr::Call { f, base } => {
                    let cfr = *(*vmctx).funcs.add(f as usize);
                    call!(cfr, base);
                }
                Instr::CallIndirect { base, ty, table } => {
                    let tab = *(*vmctx).tables.add(table as usize);
                    let m = (*vmctx).interp as *const InterpModule;
                    let np = *(&(*m).type_params).get_unchecked(ty as usize);
                    let i = r!(base + np) as u32;
                    if i as u64 >= (*tab).len {
                        trap!(TrapCode::UndefinedElement);
                    }
                    let cfr = *(*tab).elems.add(i as usize) as *const VmFuncRef;
                    if cfr.is_null() {
                        trap!(Trap::uninitialized(i));
                    }
                    if (*cfr).type_id != *(*vmctx).type_ids.add(ty as usize) {
                        trap!(TrapCode::IndirectCallTypeMismatch);
                    }
                    call!(cfr, base);
                }
                Instr::Fuel { n } => {
                    (*rt).fuel -= n as i64;
                    if (*rt).fuel < 0 {
                        trap!(TrapCode::OutOfFuel);
                    }
                }
                Instr::Copy { d, s } => w!(d, r!(s)),
                Instr::Const32 { d, v } => w!(d, v as u64),
                Instr::Const64 { d, v } => w!(d, v),
                Instr::Select { d, b, c } => {
                    if r!(c) as u32 == 0 {
                        w!(d, r!(b));
                    }
                }
                Instr::GlobalGet { d, g } => w!(d, **(*vmctx).globals.add(g as usize)),
                Instr::GlobalSet { s, g } => **(*vmctx).globals.add(g as usize) = r!(s),
                Instr::MemorySize { d } => w!(d, msize / crate::types::PAGE_SIZE),
                Instr::MemoryGrow { d, n } => {
                    let m = (*vmctx).memory;
                    let r = (*m).grow(r!(n) as u32);
                    w!(d, r as u32 as u64);
                    reload!();
                }
                Instr::MemoryCopy { base } => {
                    let (dst, src, n) = (r!(base) as u32, r!(base + 1) as u32, r!(base + 2) as u32);
                    if let Err(t) = crate::runtime::ops::memory_copy(vmctx, dst, src, n) {
                        trap!(t);
                    }
                }
                Instr::MemoryFill { base } => {
                    let (dst, val, n) = (r!(base) as u32, r!(base + 1) as u32, r!(base + 2) as u32);
                    if let Err(t) = crate::runtime::ops::memory_fill(vmctx, dst, val as u8, n) {
                        trap!(t);
                    }
                }
                Instr::MemoryInit { base, seg } => {
                    let (dst, src, n) = (r!(base) as u32, r!(base + 1) as u32, r!(base + 2) as u32);
                    if let Err(t) = (*store).memory_init(vmctx, seg, dst, src, n) {
                        trap!(t);
                    }
                }
                Instr::DataDrop { seg } => (*store).data_drop(vmctx, seg),
                Instr::TableGet { d, i, t } => {
                    let tab = *(*vmctx).tables.add(t as usize);
                    let i = r!(i) as u32;
                    match (*tab).get(i) {
                        Some(v) => w!(d, v),
                        None => trap!(TrapCode::TableOutOfBounds),
                    }
                }
                Instr::TableSet { i, v, t } => {
                    let tab = *(*vmctx).tables.add(t as usize);
                    if !(*tab).set(r!(i) as u32, r!(v)) {
                        trap!(TrapCode::TableOutOfBounds);
                    }
                }
                Instr::TableSize { d, t } => {
                    let tab = *(*vmctx).tables.add(t as usize);
                    w!(d, (*tab).size() as u64);
                }
                Instr::TableGrow { base, t } => {
                    let tab = *(*vmctx).tables.add(t as usize);
                    let r = (*tab).grow(r!(base + 1) as u32, r!(base));
                    w!(base, r as u32 as u64);
                }
                Instr::TableFill { base, t } => {
                    let (i, v, n) = (r!(base) as u32, r!(base + 1), r!(base + 2) as u32);
                    if let Err(e) = crate::runtime::ops::table_fill(vmctx, t, i, v, n) {
                        trap!(e);
                    }
                }
                Instr::TableCopy { base, dst, src } => {
                    let (d, s, n) = (r!(base) as u32, r!(base + 1) as u32, r!(base + 2) as u32);
                    if let Err(e) = crate::runtime::ops::table_copy(vmctx, dst, src, d, s, n) {
                        trap!(e);
                    }
                }
                Instr::TableInit { base, seg, t } => {
                    let (d, s, n) = (r!(base) as u32, r!(base + 1) as u32, r!(base + 2) as u32);
                    if let Err(e) = (*store).table_init(vmctx, t, seg, d, s, n) {
                        trap!(e);
                    }
                }
                Instr::ElemDrop { seg } => (*store).elem_drop(vmctx, seg),
                Instr::RefFunc { d, f } => w!(d, *(*vmctx).funcs.add(f as usize) as u64),
            }
        }
    }
}
