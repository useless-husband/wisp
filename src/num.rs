//! Semantics of the numeric instructions on untyped 64-bit slots.
//!
//! Values are kept as raw bits: `i32` and `f32` in the low 32 bits (upper bits zero),
//! `i64` and `f64` in all 64. Float operations go through the host FPU, which on AArch64
//! (and x86-64 SSE) propagates NaN payloads and produces the canonical NaN for invalid
//! operations, which is what the specification's NaN rules allow. Sign operations
//! (`abs`, `neg`, `copysign`) are bitwise and never touch payloads.

use crate::binary::ops::NumOp;
use crate::error::TrapCode;

#[inline(always)]
pub fn w32(v: u32) -> u64 {
    v as u64
}
#[inline(always)]
fn r32(v: u64) -> u32 {
    v as u32
}
#[inline(always)]
fn f32v(v: u64) -> f32 {
    f32::from_bits(v as u32)
}
#[inline(always)]
fn f64v(v: u64) -> f64 {
    f64::from_bits(v)
}
#[inline(always)]
fn wf32(v: f32) -> u64 {
    v.to_bits() as u64
}
#[inline(always)]
fn wf64(v: f64) -> u64 {
    v.to_bits()
}
#[inline(always)]
fn b(v: bool) -> u64 {
    v as u64
}

type R = Result<u64, TrapCode>;

// ---- i32 ----
#[inline(always)]
pub fn i32_eqz(a: u64) -> u64 {
    b(r32(a) == 0)
}
#[inline(always)]
pub fn i32_eq(a: u64, c: u64) -> u64 {
    b(r32(a) == r32(c))
}
#[inline(always)]
pub fn i32_ne(a: u64, c: u64) -> u64 {
    b(r32(a) != r32(c))
}
#[inline(always)]
pub fn i32_lt_s(a: u64, c: u64) -> u64 {
    b((r32(a) as i32) < (r32(c) as i32))
}
#[inline(always)]
pub fn i32_lt_u(a: u64, c: u64) -> u64 {
    b(r32(a) < r32(c))
}
#[inline(always)]
pub fn i32_gt_s(a: u64, c: u64) -> u64 {
    b((r32(a) as i32) > (r32(c) as i32))
}
#[inline(always)]
pub fn i32_gt_u(a: u64, c: u64) -> u64 {
    b(r32(a) > r32(c))
}
#[inline(always)]
pub fn i32_le_s(a: u64, c: u64) -> u64 {
    b((r32(a) as i32) <= (r32(c) as i32))
}
#[inline(always)]
pub fn i32_le_u(a: u64, c: u64) -> u64 {
    b(r32(a) <= r32(c))
}
#[inline(always)]
pub fn i32_ge_s(a: u64, c: u64) -> u64 {
    b((r32(a) as i32) >= (r32(c) as i32))
}
#[inline(always)]
pub fn i32_ge_u(a: u64, c: u64) -> u64 {
    b(r32(a) >= r32(c))
}
#[inline(always)]
pub fn i32_clz(a: u64) -> u64 {
    r32(a).leading_zeros() as u64
}
#[inline(always)]
pub fn i32_ctz(a: u64) -> u64 {
    r32(a).trailing_zeros() as u64
}
#[inline(always)]
pub fn i32_popcnt(a: u64) -> u64 {
    r32(a).count_ones() as u64
}
#[inline(always)]
pub fn i32_add(a: u64, c: u64) -> u64 {
    w32(r32(a).wrapping_add(r32(c)))
}
#[inline(always)]
pub fn i32_sub(a: u64, c: u64) -> u64 {
    w32(r32(a).wrapping_sub(r32(c)))
}
#[inline(always)]
pub fn i32_mul(a: u64, c: u64) -> u64 {
    w32(r32(a).wrapping_mul(r32(c)))
}
#[inline(always)]
pub fn i32_div_s(a: u64, c: u64) -> R {
    let (x, y) = (r32(a) as i32, r32(c) as i32);
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    if x == i32::MIN && y == -1 {
        return Err(TrapCode::IntegerOverflow);
    }
    Ok(w32((x / y) as u32))
}
#[inline(always)]
pub fn i32_div_u(a: u64, c: u64) -> R {
    let (x, y) = (r32(a), r32(c));
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(w32(x / y))
}
#[inline(always)]
pub fn i32_rem_s(a: u64, c: u64) -> R {
    let (x, y) = (r32(a) as i32, r32(c) as i32);
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(w32(x.wrapping_rem(y) as u32))
}
#[inline(always)]
pub fn i32_rem_u(a: u64, c: u64) -> R {
    let (x, y) = (r32(a), r32(c));
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(w32(x % y))
}
#[inline(always)]
pub fn i32_and(a: u64, c: u64) -> u64 {
    a & c
}
#[inline(always)]
pub fn i32_or(a: u64, c: u64) -> u64 {
    a | c
}
#[inline(always)]
pub fn i32_xor(a: u64, c: u64) -> u64 {
    a ^ c
}
#[inline(always)]
pub fn i32_shl(a: u64, c: u64) -> u64 {
    w32(r32(a).wrapping_shl(r32(c)))
}
#[inline(always)]
pub fn i32_shr_s(a: u64, c: u64) -> u64 {
    w32((r32(a) as i32).wrapping_shr(r32(c)) as u32)
}
#[inline(always)]
pub fn i32_shr_u(a: u64, c: u64) -> u64 {
    w32(r32(a).wrapping_shr(r32(c)))
}
#[inline(always)]
pub fn i32_rotl(a: u64, c: u64) -> u64 {
    w32(r32(a).rotate_left(r32(c) & 31))
}
#[inline(always)]
pub fn i32_rotr(a: u64, c: u64) -> u64 {
    w32(r32(a).rotate_right(r32(c) & 31))
}

// ---- i64 ----
#[inline(always)]
pub fn i64_eqz(a: u64) -> u64 {
    b(a == 0)
}
#[inline(always)]
pub fn i64_eq(a: u64, c: u64) -> u64 {
    b(a == c)
}
#[inline(always)]
pub fn i64_ne(a: u64, c: u64) -> u64 {
    b(a != c)
}
#[inline(always)]
pub fn i64_lt_s(a: u64, c: u64) -> u64 {
    b((a as i64) < (c as i64))
}
#[inline(always)]
pub fn i64_lt_u(a: u64, c: u64) -> u64 {
    b(a < c)
}
#[inline(always)]
pub fn i64_gt_s(a: u64, c: u64) -> u64 {
    b((a as i64) > (c as i64))
}
#[inline(always)]
pub fn i64_gt_u(a: u64, c: u64) -> u64 {
    b(a > c)
}
#[inline(always)]
pub fn i64_le_s(a: u64, c: u64) -> u64 {
    b((a as i64) <= (c as i64))
}
#[inline(always)]
pub fn i64_le_u(a: u64, c: u64) -> u64 {
    b(a <= c)
}
#[inline(always)]
pub fn i64_ge_s(a: u64, c: u64) -> u64 {
    b((a as i64) >= (c as i64))
}
#[inline(always)]
pub fn i64_ge_u(a: u64, c: u64) -> u64 {
    b(a >= c)
}
#[inline(always)]
pub fn i64_clz(a: u64) -> u64 {
    a.leading_zeros() as u64
}
#[inline(always)]
pub fn i64_ctz(a: u64) -> u64 {
    a.trailing_zeros() as u64
}
#[inline(always)]
pub fn i64_popcnt(a: u64) -> u64 {
    a.count_ones() as u64
}
#[inline(always)]
pub fn i64_add(a: u64, c: u64) -> u64 {
    a.wrapping_add(c)
}
#[inline(always)]
pub fn i64_sub(a: u64, c: u64) -> u64 {
    a.wrapping_sub(c)
}
#[inline(always)]
pub fn i64_mul(a: u64, c: u64) -> u64 {
    a.wrapping_mul(c)
}
#[inline(always)]
pub fn i64_div_s(a: u64, c: u64) -> R {
    let (x, y) = (a as i64, c as i64);
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    if x == i64::MIN && y == -1 {
        return Err(TrapCode::IntegerOverflow);
    }
    Ok((x / y) as u64)
}
#[inline(always)]
pub fn i64_div_u(a: u64, c: u64) -> R {
    if c == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(a / c)
}
#[inline(always)]
pub fn i64_rem_s(a: u64, c: u64) -> R {
    let (x, y) = (a as i64, c as i64);
    if y == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(x.wrapping_rem(y) as u64)
}
#[inline(always)]
pub fn i64_rem_u(a: u64, c: u64) -> R {
    if c == 0 {
        return Err(TrapCode::IntegerDivideByZero);
    }
    Ok(a % c)
}
#[inline(always)]
pub fn i64_and(a: u64, c: u64) -> u64 {
    a & c
}
#[inline(always)]
pub fn i64_or(a: u64, c: u64) -> u64 {
    a | c
}
#[inline(always)]
pub fn i64_xor(a: u64, c: u64) -> u64 {
    a ^ c
}
#[inline(always)]
pub fn i64_shl(a: u64, c: u64) -> u64 {
    a.wrapping_shl(c as u32)
}
#[inline(always)]
pub fn i64_shr_s(a: u64, c: u64) -> u64 {
    (a as i64).wrapping_shr(c as u32) as u64
}
#[inline(always)]
pub fn i64_shr_u(a: u64, c: u64) -> u64 {
    a.wrapping_shr(c as u32)
}
#[inline(always)]
pub fn i64_rotl(a: u64, c: u64) -> u64 {
    a.rotate_left((c & 63) as u32)
}
#[inline(always)]
pub fn i64_rotr(a: u64, c: u64) -> u64 {
    a.rotate_right((c & 63) as u32)
}

// ---- floats ----
macro_rules! fcmp {
    ($($name:ident, $conv:ident, $op:tt;)*) => {
        $(#[inline(always)] pub fn $name(a: u64, c: u64) -> u64 { b($conv(a) $op $conv(c)) })*
    };
}
fcmp! {
    f32_eq, f32v, ==; f32_ne, f32v, !=; f32_lt, f32v, <; f32_gt, f32v, >; f32_le, f32v, <=; f32_ge, f32v, >=;
    f64_eq, f64v, ==; f64_ne, f64v, !=; f64_lt, f64v, <; f64_gt, f64v, >; f64_le, f64v, <=; f64_ge, f64v, >=;
}

#[inline(always)]
pub fn f32_abs(a: u64) -> u64 {
    a & 0x7FFF_FFFF
}
#[inline(always)]
pub fn f32_neg(a: u64) -> u64 {
    (a ^ 0x8000_0000) & 0xFFFF_FFFF
}
#[inline(always)]
pub fn f32_copysign(a: u64, c: u64) -> u64 {
    (a & 0x7FFF_FFFF) | (c & 0x8000_0000)
}
#[inline(always)]
pub fn f64_abs(a: u64) -> u64 {
    a & 0x7FFF_FFFF_FFFF_FFFF
}
#[inline(always)]
pub fn f64_neg(a: u64) -> u64 {
    a ^ 0x8000_0000_0000_0000
}
#[inline(always)]
pub fn f64_copysign(a: u64, c: u64) -> u64 {
    (a & 0x7FFF_FFFF_FFFF_FFFF) | (c & 0x8000_0000_0000_0000)
}

macro_rules! funop {
    ($($name:ident, $conv:ident, $w:ident, |$x:ident| $e:expr;)*) => {
        $(#[inline(always)] pub fn $name(a: u64) -> u64 { let $x = $conv(a); $w($e) })*
    };
}
funop! {
    f32_ceil, f32v, wf32, |x| x.ceil();
    f32_floor, f32v, wf32, |x| x.floor();
    f32_trunc, f32v, wf32, |x| x.trunc();
    f32_nearest, f32v, wf32, |x| x.round_ties_even();
    f32_sqrt, f32v, wf32, |x| x.sqrt();
    f64_ceil, f64v, wf64, |x| x.ceil();
    f64_floor, f64v, wf64, |x| x.floor();
    f64_trunc, f64v, wf64, |x| x.trunc();
    f64_nearest, f64v, wf64, |x| x.round_ties_even();
    f64_sqrt, f64v, wf64, |x| x.sqrt();
}

macro_rules! fbinop {
    ($($name:ident, $conv:ident, $w:ident, |$x:ident, $y:ident| $e:expr;)*) => {
        $(#[inline(always)] pub fn $name(a: u64, c: u64) -> u64 { let $x = $conv(a); let $y = $conv(c); $w($e) })*
    };
}
fbinop! {
    f32_add, f32v, wf32, |x, y| x + y;
    f32_sub, f32v, wf32, |x, y| x - y;
    f32_mul, f32v, wf32, |x, y| x * y;
    f32_div, f32v, wf32, |x, y| x / y;
    f64_add, f64v, wf64, |x, y| x + y;
    f64_sub, f64v, wf64, |x, y| x - y;
    f64_mul, f64v, wf64, |x, y| x * y;
    f64_div, f64v, wf64, |x, y| x / y;
}

#[inline(always)]
pub fn f32_min(a: u64, c: u64) -> u64 {
    let (x, y) = (f32v(a), f32v(c));
    if x.is_nan() || y.is_nan() {
        return wf32(x + y);
    }
    if x == y {
        return (a | c) & 0xFFFF_FFFF;
    }
    wf32(if x < y { x } else { y })
}
#[inline(always)]
pub fn f32_max(a: u64, c: u64) -> u64 {
    let (x, y) = (f32v(a), f32v(c));
    if x.is_nan() || y.is_nan() {
        return wf32(x + y);
    }
    if x == y {
        return a & c;
    }
    wf32(if x > y { x } else { y })
}
#[inline(always)]
pub fn f64_min(a: u64, c: u64) -> u64 {
    let (x, y) = (f64v(a), f64v(c));
    if x.is_nan() || y.is_nan() {
        return wf64(x + y);
    }
    if x == y {
        return a | c;
    }
    wf64(if x < y { x } else { y })
}
#[inline(always)]
pub fn f64_max(a: u64, c: u64) -> u64 {
    let (x, y) = (f64v(a), f64v(c));
    if x.is_nan() || y.is_nan() {
        return wf64(x + y);
    }
    if x == y {
        return a & c;
    }
    wf64(if x > y { x } else { y })
}

// ---- conversions ----
#[inline(always)]
pub fn i32_wrap_i64(a: u64) -> u64 {
    a & 0xFFFF_FFFF
}
#[inline(always)]
pub fn i64_extend_i32_s(a: u64) -> u64 {
    r32(a) as i32 as i64 as u64
}
#[inline(always)]
pub fn i64_extend_i32_u(a: u64) -> u64 {
    a & 0xFFFF_FFFF
}
#[inline(always)]
pub fn i32_extend8_s(a: u64) -> u64 {
    w32(a as u8 as i8 as i32 as u32)
}
#[inline(always)]
pub fn i32_extend16_s(a: u64) -> u64 {
    w32(a as u16 as i16 as i32 as u32)
}
#[inline(always)]
pub fn i64_extend8_s(a: u64) -> u64 {
    a as u8 as i8 as i64 as u64
}
#[inline(always)]
pub fn i64_extend16_s(a: u64) -> u64 {
    a as u16 as i16 as i64 as u64
}
#[inline(always)]
pub fn i64_extend32_s(a: u64) -> u64 {
    a as u32 as i32 as i64 as u64
}

/// Trapping float-to-int conversion. `lo`/`hi` are exclusive bounds on the float value.
macro_rules! trunc {
    ($($name:ident, $conv:ident, $lo:expr, $hi:expr, $ity:ty, $out:expr;)*) => {
        $(#[inline(always)] pub fn $name(a: u64) -> R {
            let x = $conv(a);
            if x.is_nan() {
                return Err(TrapCode::InvalidConversionToInteger);
            }
            if !(x > $lo && x < $hi) {
                return Err(TrapCode::IntegerOverflow);
            }
            let v = x as $ity;
            Ok($out(v))
        })*
    };
}
trunc! {
    i32_trunc_f32_s, f32v, -2147483904.0f32, 2147483648.0f32, i32, |v: i32| w32(v as u32);
    i32_trunc_f32_u, f32v, -1.0f32, 4294967296.0f32, u32, w32;
    i32_trunc_f64_s, f64v, -2147483649.0f64, 2147483648.0f64, i32, |v: i32| w32(v as u32);
    i32_trunc_f64_u, f64v, -1.0f64, 4294967296.0f64, u32, w32;
    i64_trunc_f32_s, f32v, -9223373136366403584.0f32, 9223372036854775808.0f32, i64, |v: i64| v as u64;
    i64_trunc_f32_u, f32v, -1.0f32, 18446744073709551616.0f32, u64, |v: u64| v;
    i64_trunc_f64_s, f64v, -9223372036854777856.0f64, 9223372036854775808.0f64, i64, |v: i64| v as u64;
    i64_trunc_f64_u, f64v, -1.0f64, 18446744073709551616.0f64, u64, |v: u64| v;
}

macro_rules! conv {
    ($($name:ident, |$x:ident| $e:expr;)*) => {
        $(#[inline(always)] pub fn $name($x: u64) -> u64 { $e })*
    };
}
conv! {
    // Saturating truncation: Rust `as` saturates and maps NaN to 0, exactly the Wasm rule.
    i32_trunc_sat_f32_s, |a| w32(f32v(a) as i32 as u32);
    i32_trunc_sat_f32_u, |a| w32(f32v(a) as u32);
    i32_trunc_sat_f64_s, |a| w32(f64v(a) as i32 as u32);
    i32_trunc_sat_f64_u, |a| w32(f64v(a) as u32);
    i64_trunc_sat_f32_s, |a| f32v(a) as i64 as u64;
    i64_trunc_sat_f32_u, |a| f32v(a) as u64;
    i64_trunc_sat_f64_s, |a| f64v(a) as i64 as u64;
    i64_trunc_sat_f64_u, |a| f64v(a) as u64;
    f32_convert_i32_s, |a| wf32(r32(a) as i32 as f32);
    f32_convert_i32_u, |a| wf32(r32(a) as f32);
    f32_convert_i64_s, |a| wf32(a as i64 as f32);
    f32_convert_i64_u, |a| wf32(a as f32);
    f32_demote_f64, |a| wf32(f64v(a) as f32);
    f64_convert_i32_s, |a| wf64(r32(a) as i32 as f64);
    f64_convert_i32_u, |a| wf64(r32(a) as f64);
    f64_convert_i64_s, |a| wf64(a as i64 as f64);
    f64_convert_i64_u, |a| wf64(a as f64);
    f64_promote_f32, |a| wf64(f32v(a) as f64);
    reinterpret32, |a| a & 0xFFFF_FFFF;
    reinterpret64, |a| a;
}

/// Evaluate any numeric operator (slow path: tests and constant folding).
pub fn eval(op: NumOp, a: u64, c: u64) -> R {
    use NumOp::*;
    Ok(match op {
        I32Eqz => i32_eqz(a),
        I32Eq => i32_eq(a, c),
        I32Ne => i32_ne(a, c),
        I32LtS => i32_lt_s(a, c),
        I32LtU => i32_lt_u(a, c),
        I32GtS => i32_gt_s(a, c),
        I32GtU => i32_gt_u(a, c),
        I32LeS => i32_le_s(a, c),
        I32LeU => i32_le_u(a, c),
        I32GeS => i32_ge_s(a, c),
        I32GeU => i32_ge_u(a, c),
        I64Eqz => i64_eqz(a),
        I64Eq => i64_eq(a, c),
        I64Ne => i64_ne(a, c),
        I64LtS => i64_lt_s(a, c),
        I64LtU => i64_lt_u(a, c),
        I64GtS => i64_gt_s(a, c),
        I64GtU => i64_gt_u(a, c),
        I64LeS => i64_le_s(a, c),
        I64LeU => i64_le_u(a, c),
        I64GeS => i64_ge_s(a, c),
        I64GeU => i64_ge_u(a, c),
        F32Eq => f32_eq(a, c),
        F32Ne => f32_ne(a, c),
        F32Lt => f32_lt(a, c),
        F32Gt => f32_gt(a, c),
        F32Le => f32_le(a, c),
        F32Ge => f32_ge(a, c),
        F64Eq => f64_eq(a, c),
        F64Ne => f64_ne(a, c),
        F64Lt => f64_lt(a, c),
        F64Gt => f64_gt(a, c),
        F64Le => f64_le(a, c),
        F64Ge => f64_ge(a, c),
        I32Clz => i32_clz(a),
        I32Ctz => i32_ctz(a),
        I32Popcnt => i32_popcnt(a),
        I32Add => i32_add(a, c),
        I32Sub => i32_sub(a, c),
        I32Mul => i32_mul(a, c),
        I32DivS => i32_div_s(a, c)?,
        I32DivU => i32_div_u(a, c)?,
        I32RemS => i32_rem_s(a, c)?,
        I32RemU => i32_rem_u(a, c)?,
        I32And => i32_and(a, c),
        I32Or => i32_or(a, c),
        I32Xor => i32_xor(a, c),
        I32Shl => i32_shl(a, c),
        I32ShrS => i32_shr_s(a, c),
        I32ShrU => i32_shr_u(a, c),
        I32Rotl => i32_rotl(a, c),
        I32Rotr => i32_rotr(a, c),
        I64Clz => i64_clz(a),
        I64Ctz => i64_ctz(a),
        I64Popcnt => i64_popcnt(a),
        I64Add => i64_add(a, c),
        I64Sub => i64_sub(a, c),
        I64Mul => i64_mul(a, c),
        I64DivS => i64_div_s(a, c)?,
        I64DivU => i64_div_u(a, c)?,
        I64RemS => i64_rem_s(a, c)?,
        I64RemU => i64_rem_u(a, c)?,
        I64And => i64_and(a, c),
        I64Or => i64_or(a, c),
        I64Xor => i64_xor(a, c),
        I64Shl => i64_shl(a, c),
        I64ShrS => i64_shr_s(a, c),
        I64ShrU => i64_shr_u(a, c),
        I64Rotl => i64_rotl(a, c),
        I64Rotr => i64_rotr(a, c),
        F32Abs => f32_abs(a),
        F32Neg => f32_neg(a),
        F32Ceil => f32_ceil(a),
        F32Floor => f32_floor(a),
        F32Trunc => f32_trunc(a),
        F32Nearest => f32_nearest(a),
        F32Sqrt => f32_sqrt(a),
        F32Add => f32_add(a, c),
        F32Sub => f32_sub(a, c),
        F32Mul => f32_mul(a, c),
        F32Div => f32_div(a, c),
        F32Min => f32_min(a, c),
        F32Max => f32_max(a, c),
        F32Copysign => f32_copysign(a, c),
        F64Abs => f64_abs(a),
        F64Neg => f64_neg(a),
        F64Ceil => f64_ceil(a),
        F64Floor => f64_floor(a),
        F64Trunc => f64_trunc(a),
        F64Nearest => f64_nearest(a),
        F64Sqrt => f64_sqrt(a),
        F64Add => f64_add(a, c),
        F64Sub => f64_sub(a, c),
        F64Mul => f64_mul(a, c),
        F64Div => f64_div(a, c),
        F64Min => f64_min(a, c),
        F64Max => f64_max(a, c),
        F64Copysign => f64_copysign(a, c),
        I32WrapI64 => i32_wrap_i64(a),
        I32TruncF32S => i32_trunc_f32_s(a)?,
        I32TruncF32U => i32_trunc_f32_u(a)?,
        I32TruncF64S => i32_trunc_f64_s(a)?,
        I32TruncF64U => i32_trunc_f64_u(a)?,
        I64ExtendI32S => i64_extend_i32_s(a),
        I64ExtendI32U => i64_extend_i32_u(a),
        I64TruncF32S => i64_trunc_f32_s(a)?,
        I64TruncF32U => i64_trunc_f32_u(a)?,
        I64TruncF64S => i64_trunc_f64_s(a)?,
        I64TruncF64U => i64_trunc_f64_u(a)?,
        F32ConvertI32S => f32_convert_i32_s(a),
        F32ConvertI32U => f32_convert_i32_u(a),
        F32ConvertI64S => f32_convert_i64_s(a),
        F32ConvertI64U => f32_convert_i64_u(a),
        F32DemoteF64 => f32_demote_f64(a),
        F64ConvertI32S => f64_convert_i32_s(a),
        F64ConvertI32U => f64_convert_i32_u(a),
        F64ConvertI64S => f64_convert_i64_s(a),
        F64ConvertI64U => f64_convert_i64_u(a),
        F64PromoteF32 => f64_promote_f32(a),
        I32ReinterpretF32 => reinterpret32(a),
        I64ReinterpretF64 => reinterpret64(a),
        F32ReinterpretI32 => reinterpret32(a),
        F64ReinterpretI64 => reinterpret64(a),
        I32Extend8S => i32_extend8_s(a),
        I32Extend16S => i32_extend16_s(a),
        I64Extend8S => i64_extend8_s(a),
        I64Extend16S => i64_extend16_s(a),
        I64Extend32S => i64_extend32_s(a),
        I32TruncSatF32S => i32_trunc_sat_f32_s(a),
        I32TruncSatF32U => i32_trunc_sat_f32_u(a),
        I32TruncSatF64S => i32_trunc_sat_f64_s(a),
        I32TruncSatF64U => i32_trunc_sat_f64_u(a),
        I64TruncSatF32S => i64_trunc_sat_f32_s(a),
        I64TruncSatF32U => i64_trunc_sat_f32_u(a),
        I64TruncSatF64S => i64_trunc_sat_f64_s(a),
        I64TruncSatF64U => i64_trunc_sat_f64_u(a),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trunc_bounds() {
        let f = |x: f32| x.to_bits() as u64;
        let d = |x: f64| x.to_bits();
        assert_eq!(i32_trunc_f32_s(f(-2147483648.0)), Ok(0x8000_0000));
        assert_eq!(
            i32_trunc_f32_s(f(2147483648.0)),
            Err(TrapCode::IntegerOverflow)
        );
        assert_eq!(i32_trunc_f32_u(f(-0.9)), Ok(0));
        assert_eq!(i32_trunc_f32_u(f(-1.0)), Err(TrapCode::IntegerOverflow));
        assert_eq!(i32_trunc_f64_s(d(-2147483648.9)), Ok(0x8000_0000));
        assert_eq!(
            i32_trunc_f64_s(d(-2147483649.0)),
            Err(TrapCode::IntegerOverflow)
        );
        assert_eq!(
            i64_trunc_f64_s(d(-9223372036854775808.0)),
            Ok(i64::MIN as u64)
        );
        assert_eq!(
            i64_trunc_f32_s(f(-9223372036854775808.0)),
            Ok(i64::MIN as u64)
        );
        assert_eq!(
            i64_trunc_f64_u(d(f64::NAN)),
            Err(TrapCode::InvalidConversionToInteger)
        );
        assert_eq!(i64_trunc_sat_f64_u(d(-5.0)), 0);
        assert_eq!(i32_trunc_sat_f32_s(f(f32::NAN)), 0);
    }

    #[test]
    fn min_max_zero_and_nan() {
        let p = 0.0f32.to_bits() as u64;
        let n = (-0.0f32).to_bits() as u64;
        assert_eq!(f32_min(p, n), n);
        assert_eq!(f32_max(n, p), p);
        let nan = f32_min(f32::NAN.to_bits() as u64, p);
        assert!(f32::from_bits(nan as u32).is_nan());
        assert_eq!(
            f64_min(0f64.to_bits(), (-0f64).to_bits()),
            (-0f64).to_bits()
        );
    }

    #[test]
    fn sign_ops_keep_payload() {
        let snan = 0x7FA0_0001u64;
        assert_eq!(f32_neg(snan), 0xFFA0_0001);
        assert_eq!(f32_abs(0xFFA0_0001), snan);
        assert_eq!(f32_copysign(snan, 0x8000_0000), 0xFFA0_0001);
    }

    #[test]
    fn division_traps() {
        assert_eq!(
            i32_div_s(0x8000_0000, 0xFFFF_FFFF),
            Err(TrapCode::IntegerOverflow)
        );
        assert_eq!(i32_rem_s(0x8000_0000, 0xFFFF_FFFF), Ok(0));
        assert_eq!(i64_div_u(1, 0), Err(TrapCode::IntegerDivideByZero));
    }
}
