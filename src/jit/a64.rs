//! An AArch64 instruction encoder: the subset the baseline compiler emits.
//!
//! Register numbers are 0..=31; 31 means `sp` or `xzr`/`wzr` depending on the instruction,
//! exactly as in the architecture. `sf` selects 64-bit (`true`) or 32-bit operation.

pub const SP: u8 = 31;
pub const ZR: u8 = 31;
pub const FP: u8 = 29;
pub const LR: u8 = 30;

/// Condition codes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

impl Cond {
    pub fn invert(self) -> Cond {
        use Cond::*;
        match self {
            Eq => Ne,
            Ne => Eq,
            Hs => Lo,
            Lo => Hs,
            Mi => Pl,
            Pl => Mi,
            Vs => Vc,
            Vc => Vs,
            Hi => Ls,
            Ls => Hi,
            Ge => Lt,
            Lt => Ge,
            Gt => Le,
            Le => Gt,
        }
    }
}

/// A position in the code that branches can target.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Label(pub u32);

#[derive(Copy, Clone, Debug)]
enum Fixup {
    /// B / BL: imm26.
    Imm26,
    /// B.cond / CBZ / CBNZ: imm19 at bit 5.
    Imm19,
    /// TBZ / TBNZ: imm14 at bit 5.
    Imm14,
}

/// Accumulates machine code with label fixups.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u32>,
    labels: Vec<Option<u32>>,
    fixups: Vec<(u32, Label, Fixup)>,
}

#[inline]
fn r(x: u8) -> u32 {
    debug_assert!(x < 32);
    x as u32 & 31
}

#[inline]
fn sfb(sf: bool) -> u32 {
    (sf as u32) << 31
}

/// Encode a logical (bitmask) immediate: returns `N:immr:imms` (13 bits) if `value` is
/// representable for an operation of `width` (32 or 64) bits.
pub fn logical_imm(value: u64, width: u32) -> Option<u32> {
    let v = if width == 32 {
        let v = value & 0xFFFF_FFFF;
        v | (v << 32)
    } else {
        value
    };
    if v == 0 || v == u64::MAX {
        return None;
    }
    // Smallest element size that repeats across the 64 bits.
    let mut size = 64u32;
    while size > 2 {
        let half = size / 2;
        let mask = (1u64 << half) - 1;
        if (v & mask) != ((v >> half) & mask) {
            break;
        }
        size = half;
    }
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let elem = v & mask;
    let ones = elem.count_ones();
    let pattern = if ones == 64 {
        u64::MAX
    } else {
        (1u64 << ones) - 1
    };
    let rotr = |x: u64, n: u32| -> u64 {
        if n == 0 {
            x
        } else {
            ((x >> n) | (x << (size - n))) & mask
        }
    };
    let rot = (0..size).find(|&n| rotr(elem, n) == pattern)?;
    let immr = (size - rot) % size;
    let imms = ((!(size - 1) << 1) | (ones - 1)) & 0x3F;
    let n = (size == 64) as u32;
    Some((n << 12) | (immr << 6) | imms)
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pos(&self) -> u32 {
        self.code.len() as u32
    }

    pub fn emit(&mut self, i: u32) {
        self.code.push(i);
    }

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() as u32 - 1)
    }

    pub fn bind(&mut self, l: Label) {
        debug_assert!(self.labels[l.0 as usize].is_none(), "label bound twice");
        self.labels[l.0 as usize] = Some(self.pos());
    }

    pub fn is_bound(&self, l: Label) -> bool {
        self.labels[l.0 as usize].is_some()
    }

    pub fn label_pos(&self, l: Label) -> Option<u32> {
        self.labels[l.0 as usize]
    }

    /// Code length, fixups and label state, for rolling back a function that fails.
    pub fn checkpoint(&self) -> (usize, usize) {
        (self.code.len(), self.fixups.len())
    }

    pub fn rollback(&mut self, cp: (usize, usize)) {
        self.code.truncate(cp.0);
        self.fixups.truncate(cp.1);
        for l in self.labels.iter_mut() {
            if l.is_some_and(|p| p as usize >= cp.0) {
                *l = None;
            }
        }
    }

    /// Resolve all label references. Fails on an unbound label or an out-of-range branch.
    pub fn finish(&mut self) -> Result<(), String> {
        for &(at, l, kind) in &self.fixups {
            let target = self.labels[l.0 as usize].ok_or("unbound label")?;
            let delta = target as i64 - at as i64;
            let ins = &mut self.code[at as usize];
            let (bits, shift) = match kind {
                Fixup::Imm26 => (26, 0),
                Fixup::Imm19 => (19, 5),
                Fixup::Imm14 => (14, 5),
            };
            if !(-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(&delta) {
                return Err(format!("branch of {delta} instructions out of range"));
            }
            *ins |= ((delta as u32) & ((1u32 << bits) - 1)) << shift;
        }
        self.fixups.clear();
        Ok(())
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    // ---- branches ----

    pub fn b(&mut self, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm26));
        self.emit(0x1400_0000);
    }

    pub fn bl(&mut self, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm26));
        self.emit(0x9400_0000);
    }

    /// BL to an instruction index fixed later by the caller (cross-function calls).
    pub fn bl_placeholder(&mut self) -> u32 {
        let p = self.pos();
        self.emit(0x9400_0000);
        p
    }

    pub fn patch_bl(&mut self, at: u32, target: u32) {
        let delta = target as i64 - at as i64;
        assert!((-(1 << 25)..(1 << 25)).contains(&delta));
        self.code[at as usize] = 0x9400_0000 | ((delta as u32) & 0x03FF_FFFF);
    }

    pub fn b_cond(&mut self, c: Cond, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm19));
        self.emit(0x5400_0000 | c as u32);
    }

    pub fn cbz(&mut self, sf: bool, rt: u8, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm19));
        self.emit(sfb(sf) | 0x3400_0000 | r(rt));
    }

    pub fn cbnz(&mut self, sf: bool, rt: u8, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm19));
        self.emit(sfb(sf) | 0x3500_0000 | r(rt));
    }

    pub fn tbnz(&mut self, rt: u8, bit: u32, l: Label) {
        self.fixups.push((self.pos(), l, Fixup::Imm14));
        self.emit(((bit >> 5) << 31) | 0x3700_0000 | ((bit & 31) << 19) | r(rt));
    }

    pub fn br(&mut self, rn: u8) {
        self.emit(0xD61F_0000 | (r(rn) << 5));
    }

    pub fn blr(&mut self, rn: u8) {
        self.emit(0xD63F_0000 | (r(rn) << 5));
    }

    pub fn ret(&mut self) {
        self.emit(0xD65F_03C0);
    }

    pub fn brk(&mut self, imm: u16) {
        self.emit(0xD420_0000 | ((imm as u32) << 5));
    }

    // ---- moves ----

    pub fn movz(&mut self, sf: bool, rd: u8, imm16: u16, shift: u32) {
        self.emit(sfb(sf) | 0x5280_0000 | ((shift / 16) << 21) | ((imm16 as u32) << 5) | r(rd));
    }

    pub fn movn(&mut self, sf: bool, rd: u8, imm16: u16, shift: u32) {
        self.emit(sfb(sf) | 0x1280_0000 | ((shift / 16) << 21) | ((imm16 as u32) << 5) | r(rd));
    }

    pub fn movk(&mut self, sf: bool, rd: u8, imm16: u16, shift: u32) {
        self.emit(sfb(sf) | 0x7280_0000 | ((shift / 16) << 21) | ((imm16 as u32) << 5) | r(rd));
    }

    /// Load any constant into `rd` with the shortest MOVZ/MOVN/MOVK/ORR sequence.
    pub fn mov_imm(&mut self, sf: bool, rd: u8, value: u64) {
        let v = if sf { value } else { value & 0xFFFF_FFFF };
        let width = if sf { 64 } else { 32 };
        let chunks = width / 16;
        let half = |i: u32| ((v >> (16 * i)) & 0xFFFF) as u16;
        let zeros = (0..chunks).filter(|&i| half(i) == 0).count() as u32;
        let ones = (0..chunks).filter(|&i| half(i) == 0xFFFF).count() as u32;
        if zeros == chunks {
            self.movz(sf, rd, 0, 0);
            return;
        }
        if ones > zeros {
            // MOVN for mostly-ones values.
            let mut first = true;
            for i in 0..chunks {
                let h = half(i);
                if h == 0xFFFF {
                    continue;
                }
                if first {
                    self.movn(sf, rd, !h, 16 * i);
                    first = false;
                } else {
                    self.movk(sf, rd, h, 16 * i);
                }
            }
            if first {
                self.movn(sf, rd, 0, 0);
            }
            return;
        }
        if chunks - zeros > 2
            && let Some(enc) = logical_imm(v, width)
        {
            // ORR rd, zr, #imm
            self.emit(sfb(sf) | 0x3200_0000 | (enc << 10) | (r(ZR) << 5) | r(rd));
            return;
        }
        let mut first = true;
        for i in 0..chunks {
            let h = half(i);
            if h == 0 {
                continue;
            }
            if first {
                self.movz(sf, rd, h, 16 * i);
                first = false;
            } else {
                self.movk(sf, rd, h, 16 * i);
            }
        }
    }

    /// `mov rd, rm` between general registers (not SP).
    pub fn mov(&mut self, sf: bool, rd: u8, rm: u8) {
        self.orr(sf, rd, ZR, rm);
    }

    /// `mov rd, sp` / `mov sp, rn` (ADD #0).
    pub fn mov_sp(&mut self, rd: u8, rn: u8) {
        self.add_imm(true, rd, rn, 0);
    }

    // ---- arithmetic ----

    fn addsub_imm(&mut self, base: u32, sf: bool, rd: u8, rn: u8, imm: u32) {
        let (imm12, sh) = if imm < 4096 {
            (imm, 0)
        } else {
            assert!(
                imm & 0xFFF == 0 && imm < (1 << 24),
                "immediate {imm:#x} not encodable"
            );
            (imm >> 12, 1)
        };
        self.emit(sfb(sf) | base | (sh << 22) | (imm12 << 10) | (r(rn) << 5) | r(rd));
    }

    /// Whether `imm` fits an ADD/SUB immediate.
    pub fn addsub_imm_ok(imm: u64) -> bool {
        imm < 4096 || (imm & 0xFFF == 0 && imm < (1 << 24))
    }

    pub fn add_imm(&mut self, sf: bool, rd: u8, rn: u8, imm: u32) {
        self.addsub_imm(0x1100_0000, sf, rd, rn, imm);
    }

    pub fn adds_imm(&mut self, sf: bool, rd: u8, rn: u8, imm: u32) {
        self.addsub_imm(0x3100_0000, sf, rd, rn, imm);
    }

    pub fn sub_imm(&mut self, sf: bool, rd: u8, rn: u8, imm: u32) {
        self.addsub_imm(0x5100_0000, sf, rd, rn, imm);
    }

    pub fn subs_imm(&mut self, sf: bool, rd: u8, rn: u8, imm: u32) {
        self.addsub_imm(0x7100_0000, sf, rd, rn, imm);
    }

    pub fn cmp_imm(&mut self, sf: bool, rn: u8, imm: u32) {
        self.subs_imm(sf, ZR, rn, imm);
    }

    pub fn cmn_imm(&mut self, sf: bool, rn: u8, imm: u32) {
        self.adds_imm(sf, ZR, rn, imm);
    }

    fn addsub_reg(&mut self, base: u32, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.emit(sfb(sf) | base | (r(rm) << 16) | (r(rn) << 5) | r(rd));
    }

    pub fn add(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.addsub_reg(0x0B00_0000, sf, rd, rn, rm);
    }

    pub fn adds(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.addsub_reg(0x2B00_0000, sf, rd, rn, rm);
    }

    pub fn sub(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.addsub_reg(0x4B00_0000, sf, rd, rn, rm);
    }

    pub fn subs(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.addsub_reg(0x6B00_0000, sf, rd, rn, rm);
    }

    pub fn cmp(&mut self, sf: bool, rn: u8, rm: u8) {
        self.subs(sf, ZR, rn, rm);
    }

    /// `add rd, rn, rm, lsl #sh`
    pub fn add_lsl(&mut self, sf: bool, rd: u8, rn: u8, rm: u8, sh: u32) {
        self.emit(sfb(sf) | 0x0B00_0000 | (r(rm) << 16) | (sh << 10) | (r(rn) << 5) | r(rd));
    }

    /// `adr rd, #off` (byte offset from this instruction).
    pub fn adr(&mut self, rd: u8, off: i32) {
        let imm = off as u32;
        self.emit(0x1000_0000 | ((imm & 3) << 29) | (((imm >> 2) & 0x7FFFF) << 5) | r(rd));
    }

    pub fn neg(&mut self, sf: bool, rd: u8, rm: u8) {
        self.sub(sf, rd, ZR, rm);
    }

    /// `add xd, xn|sp, xm` (extended register form, UXTX), usable with SP as `rn`.
    pub fn add_ext(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(0x8B20_6000 | (r(rm) << 16) | (r(rn) << 5) | r(rd));
    }

    /// `sub xd|sp, xn|sp, xm` (extended register form, UXTX).
    pub fn sub_ext(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(0xCB20_6000 | (r(rm) << 16) | (r(rn) << 5) | r(rd));
    }

    /// `cmp xn|sp, xm` (extended register form, UXTX).
    pub fn cmp_ext(&mut self, rn: u8, rm: u8) {
        self.emit(0xEB20_6000 | (r(rm) << 16) | (r(rn) << 5) | r(ZR));
    }

    fn logical_reg(&mut self, base: u32, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.emit(sfb(sf) | base | (r(rm) << 16) | (r(rn) << 5) | r(rd));
    }

    pub fn and(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.logical_reg(0x0A00_0000, sf, rd, rn, rm);
    }

    pub fn orr(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.logical_reg(0x2A00_0000, sf, rd, rn, rm);
    }

    pub fn eor(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.logical_reg(0x4A00_0000, sf, rd, rn, rm);
    }

    pub fn tst(&mut self, sf: bool, rn: u8, rm: u8) {
        self.logical_reg(0x6A00_0000, sf, ZR, rn, rm);
    }

    /// AND/ORR/EOR with a bitmask immediate; `opc` 0 = AND, 1 = ORR, 2 = EOR, 3 = ANDS.
    pub fn logical_imm(&mut self, opc: u32, sf: bool, rd: u8, rn: u8, enc: u32) {
        self.emit(sfb(sf) | 0x1200_0000 | (opc << 29) | (enc << 10) | (r(rn) << 5) | r(rd));
    }

    fn dp2(&mut self, op: u32, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.emit(sfb(sf) | 0x1AC0_0000 | (r(rm) << 16) | (op << 10) | (r(rn) << 5) | r(rd));
    }

    pub fn udiv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b000010, sf, rd, rn, rm);
    }

    pub fn sdiv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b000011, sf, rd, rn, rm);
    }

    pub fn lslv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b001000, sf, rd, rn, rm);
    }

    pub fn lsrv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b001001, sf, rd, rn, rm);
    }

    pub fn asrv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b001010, sf, rd, rn, rm);
    }

    pub fn rorv(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.dp2(0b001011, sf, rd, rn, rm);
    }

    fn dp1(&mut self, op: u32, sf: bool, rd: u8, rn: u8) {
        self.emit(sfb(sf) | 0x5AC0_0000 | (op << 10) | (r(rn) << 5) | r(rd));
    }

    pub fn rbit(&mut self, sf: bool, rd: u8, rn: u8) {
        self.dp1(0b000000, sf, rd, rn);
    }

    pub fn clz(&mut self, sf: bool, rd: u8, rn: u8) {
        self.dp1(0b000100, sf, rd, rn);
    }

    pub fn madd(&mut self, sf: bool, rd: u8, rn: u8, rm: u8, ra: u8) {
        self.emit(sfb(sf) | 0x1B00_0000 | (r(rm) << 16) | (r(ra) << 10) | (r(rn) << 5) | r(rd));
    }

    pub fn msub(&mut self, sf: bool, rd: u8, rn: u8, rm: u8, ra: u8) {
        self.emit(sfb(sf) | 0x1B00_8000 | (r(rm) << 16) | (r(ra) << 10) | (r(rn) << 5) | r(rd));
    }

    pub fn mul(&mut self, sf: bool, rd: u8, rn: u8, rm: u8) {
        self.madd(sf, rd, rn, rm, ZR);
    }

    fn bitfield(&mut self, base: u32, sf: bool, rd: u8, rn: u8, immr: u32, imms: u32) {
        let n = if sf { 1 << 22 } else { 0 };
        self.emit(sfb(sf) | base | n | (immr << 16) | (imms << 10) | (r(rn) << 5) | r(rd));
    }

    pub fn sbfm(&mut self, sf: bool, rd: u8, rn: u8, immr: u32, imms: u32) {
        self.bitfield(0x1300_0000, sf, rd, rn, immr, imms);
    }

    pub fn ubfm(&mut self, sf: bool, rd: u8, rn: u8, immr: u32, imms: u32) {
        self.bitfield(0x5300_0000, sf, rd, rn, immr, imms);
    }

    pub fn lsl_imm(&mut self, sf: bool, rd: u8, rn: u8, sh: u32) {
        let w = if sf { 64 } else { 32 };
        let sh = sh % w;
        self.ubfm(sf, rd, rn, (w - sh) % w, w - 1 - sh);
    }

    pub fn lsr_imm(&mut self, sf: bool, rd: u8, rn: u8, sh: u32) {
        let w = if sf { 64 } else { 32 };
        self.ubfm(sf, rd, rn, sh % w, w - 1);
    }

    pub fn asr_imm(&mut self, sf: bool, rd: u8, rn: u8, sh: u32) {
        let w = if sf { 64 } else { 32 };
        self.sbfm(sf, rd, rn, sh % w, w - 1);
    }

    pub fn ror_imm(&mut self, sf: bool, rd: u8, rn: u8, sh: u32) {
        let w = if sf { 64 } else { 32 };
        let base = if sf { 0x93C0_0000 } else { 0x1380_0000 };
        self.emit(base | (r(rn) << 16) | ((sh % w) << 10) | (r(rn) << 5) | r(rd));
    }

    /// Sign-extend the low `bits` (8, 16 or 32) of `rn`.
    pub fn sxt(&mut self, sf: bool, rd: u8, rn: u8, bits: u32) {
        self.sbfm(sf, rd, rn, 0, bits - 1);
    }

    /// Zero-extend the low `bits` (8 or 16) of `rn`.
    pub fn uxt(&mut self, rd: u8, rn: u8, bits: u32) {
        self.ubfm(false, rd, rn, 0, bits - 1);
    }

    pub fn csel(&mut self, sf: bool, rd: u8, rn: u8, rm: u8, c: Cond) {
        self.emit(
            sfb(sf) | 0x1A80_0000 | (r(rm) << 16) | ((c as u32) << 12) | (r(rn) << 5) | r(rd),
        );
    }

    pub fn csinc(&mut self, sf: bool, rd: u8, rn: u8, rm: u8, c: Cond) {
        self.emit(
            sfb(sf) | 0x1A80_0400 | (r(rm) << 16) | ((c as u32) << 12) | (r(rn) << 5) | r(rd),
        );
    }

    pub fn cset(&mut self, sf: bool, rd: u8, c: Cond) {
        self.csinc(sf, rd, ZR, ZR, c.invert());
    }

    // ---- loads and stores ----

    /// Unsigned-offset form: `base` is the encoding for offset 0, `scale` the access size
    /// log2. Falls back to the unscaled (LDUR/STUR) form for small unaligned offsets.
    /// Returns false if the offset cannot be encoded.
    fn ldst(&mut self, base: u32, scale: u32, rt: u8, rn: u8, off: i64) -> bool {
        if off >= 0 && off % (1 << scale) == 0 && (off >> scale) < 4096 {
            self.emit(base | (((off >> scale) as u32) << 10) | (r(rn) << 5) | r(rt));
            return true;
        }
        if (-256..256).contains(&off) {
            let unscaled = base & !(1 << 24);
            self.emit(unscaled | (((off as u32) & 0x1FF) << 12) | (r(rn) << 5) | r(rt));
            return true;
        }
        false
    }

    /// Register-offset form `[rn, rm{, lsl #scale}]` (rm 64-bit).
    fn ldst_reg(&mut self, base: u32, rt: u8, rn: u8, rm: u8, shifted: bool) {
        let enc = (base & !(1 << 24))
            | (1 << 21)
            | (0b011 << 13)
            | ((shifted as u32) << 12)
            | (0b10 << 10);
        self.emit(enc | (r(rm) << 16) | (r(rn) << 5) | r(rt));
    }

    pub fn try_ldst(&mut self, k: Mem, rt: u8, rn: u8, off: i64) -> bool {
        let (base, scale) = k.enc();
        self.ldst(base, scale, rt, rn, off)
    }

    /// Load/store with an immediate offset; `tmp` is used for offsets out of range.
    pub fn ldst_off(&mut self, k: Mem, rt: u8, rn: u8, off: i64, tmp: u8) {
        if self.try_ldst(k, rt, rn, off) {
            return;
        }
        self.mov_imm(true, tmp, off as u64);
        self.add_ext(tmp, rn, tmp);
        assert!(self.try_ldst(k, rt, tmp, 0));
    }

    /// Load/store `[rn, rm]` or `[rn, rm, lsl #size]`.
    pub fn ldst_regoff(&mut self, k: Mem, rt: u8, rn: u8, rm: u8, scaled: bool) {
        let (base, _) = k.enc();
        self.ldst_reg(base, rt, rn, rm, scaled);
    }

    fn pair(&mut self, base: u32, scale: u32, rt: u8, rt2: u8, rn: u8, off: i32) {
        let imm7 = ((off >> scale) as u32) & 0x7F;
        self.emit(base | (imm7 << 15) | (r(rt2) << 10) | (r(rn) << 5) | r(rt));
    }

    /// `stp xt, xt2, [rn, #off]!`
    pub fn stp_pre(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0xA980_0000, 3, rt, rt2, rn, off);
    }

    /// `ldp xt, xt2, [rn], #off`
    pub fn ldp_post(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0xA8C0_0000, 3, rt, rt2, rn, off);
    }

    /// `stp xt, xt2, [rn, #off]`
    pub fn stp(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0xA900_0000, 3, rt, rt2, rn, off);
    }

    /// `ldp xt, xt2, [rn, #off]`
    pub fn ldp(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0xA940_0000, 3, rt, rt2, rn, off);
    }

    /// `stp dt, dt2, [rn, #off]!`
    pub fn stp_d_pre(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0x6D80_0000, 3, rt, rt2, rn, off);
    }

    /// `ldp dt, dt2, [rn], #off`
    pub fn ldp_d_post(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0x6CC0_0000, 3, rt, rt2, rn, off);
    }

    /// `stp dt, dt2, [rn, #off]`
    pub fn stp_d(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0x6D00_0000, 3, rt, rt2, rn, off);
    }

    /// `ldp dt, dt2, [rn, #off]`
    pub fn ldp_d(&mut self, rt: u8, rt2: u8, rn: u8, off: i32) {
        self.pair(0x6D40_0000, 3, rt, rt2, rn, off);
    }

    // ---- floating point ----

    fn fp1(&mut self, dbl: bool, op: u32, rd: u8, rn: u8) {
        self.emit(0x1E20_4000 | ((dbl as u32) << 22) | (op << 15) | (r(rn) << 5) | r(rd));
    }

    pub fn fmov(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b000000, rd, rn);
    }

    pub fn fabs(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b000001, rd, rn);
    }

    pub fn fneg(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b000010, rd, rn);
    }

    pub fn fsqrt(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b000011, rd, rn);
    }

    /// FCVT: single to double when `to_double`, else double to single.
    pub fn fcvt(&mut self, to_double: bool, rd: u8, rn: u8) {
        if to_double {
            self.fp1(false, 0b000101, rd, rn);
        } else {
            self.fp1(true, 0b000100, rd, rn);
        }
    }

    pub fn frintn(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b001000, rd, rn);
    }

    pub fn frintp(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b001001, rd, rn);
    }

    pub fn frintm(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b001010, rd, rn);
    }

    pub fn frintz(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fp1(dbl, 0b001011, rd, rn);
    }

    /// FP 2-source: 0 FMUL, 1 FDIV, 2 FADD, 3 FSUB, 4 FMAX, 5 FMIN.
    pub fn fp2(&mut self, dbl: bool, op: u32, rd: u8, rn: u8, rm: u8) {
        self.emit(
            0x1E20_0800 | ((dbl as u32) << 22) | (r(rm) << 16) | (op << 12) | (r(rn) << 5) | r(rd),
        );
    }

    pub fn fcmp(&mut self, dbl: bool, rn: u8, rm: u8) {
        self.emit(0x1E20_2000 | ((dbl as u32) << 22) | (r(rm) << 16) | (r(rn) << 5));
    }

    pub fn fcsel(&mut self, dbl: bool, rd: u8, rn: u8, rm: u8, c: Cond) {
        self.emit(
            0x1E20_0C00
                | ((dbl as u32) << 22)
                | (r(rm) << 16)
                | ((c as u32) << 12)
                | (r(rn) << 5)
                | r(rd),
        );
    }

    fn fpint(&mut self, sf: bool, dbl: bool, rmode: u32, op: u32, rd: u8, rn: u8) {
        self.emit(
            sfb(sf)
                | 0x1E20_0000
                | ((dbl as u32) << 22)
                | (rmode << 19)
                | (op << 16)
                | (r(rn) << 5)
                | r(rd),
        );
    }

    /// Signed integer (`sf`: 64-bit) to float (`dbl`: double).
    pub fn scvtf(&mut self, sf: bool, dbl: bool, rd: u8, rn: u8) {
        self.fpint(sf, dbl, 0b00, 0b010, rd, rn);
    }

    pub fn ucvtf(&mut self, sf: bool, dbl: bool, rd: u8, rn: u8) {
        self.fpint(sf, dbl, 0b00, 0b011, rd, rn);
    }

    /// Float to signed integer, rounding toward zero (saturating).
    pub fn fcvtzs(&mut self, sf: bool, dbl: bool, rd: u8, rn: u8) {
        self.fpint(sf, dbl, 0b11, 0b000, rd, rn);
    }

    pub fn fcvtzu(&mut self, sf: bool, dbl: bool, rd: u8, rn: u8) {
        self.fpint(sf, dbl, 0b11, 0b001, rd, rn);
    }

    /// Move float bits to a general register (`fmov wd, sn` / `fmov xd, dn`).
    pub fn fmov_to_gpr(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fpint(dbl, dbl, 0b00, 0b110, rd, rn);
    }

    /// Move general register bits to a float register.
    pub fn fmov_from_gpr(&mut self, dbl: bool, rd: u8, rn: u8) {
        self.fpint(dbl, dbl, 0b00, 0b111, rd, rn);
    }

    /// `cnt vd.8b, vn.8b`
    pub fn cnt8b(&mut self, rd: u8, rn: u8) {
        self.emit(0x0E20_5800 | (r(rn) << 5) | r(rd));
    }

    /// `addv bd, vn.8b`
    pub fn addv8b(&mut self, rd: u8, rn: u8) {
        self.emit(0x0E31_B800 | (r(rn) << 5) | r(rd));
    }

    /// `bit vd.8b, vn.8b, vm.8b`: insert bits of vn where vm is set.
    pub fn bit8b(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(0x2EA0_1C00 | (r(rm) << 16) | (r(rn) << 5) | r(rd));
    }
}

/// Memory access kinds for loads and stores.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mem {
    LdrX,
    StrX,
    LdrW,
    StrW,
    LdrH,
    LdrShW,
    LdrShX,
    StrH,
    LdrB,
    LdrSbW,
    LdrSbX,
    StrB,
    LdrSwX,
    LdrS,
    StrS,
    LdrD,
    StrD,
}

impl Mem {
    /// (unsigned-offset encoding with offset 0, log2 of access size)
    fn enc(self) -> (u32, u32) {
        use Mem::*;
        match self {
            LdrX => (0xF940_0000, 3),
            StrX => (0xF900_0000, 3),
            LdrW => (0xB940_0000, 2),
            StrW => (0xB900_0000, 2),
            LdrH => (0x7940_0000, 1),
            LdrShW => (0x79C0_0000, 1),
            LdrShX => (0x7980_0000, 1),
            StrH => (0x7900_0000, 1),
            LdrB => (0x3940_0000, 0),
            LdrSbW => (0x39C0_0000, 0),
            LdrSbX => (0x3980_0000, 0),
            StrB => (0x3900_0000, 0),
            LdrSwX => (0xB980_0000, 2),
            LdrS => (0xBD40_0000, 2),
            StrS => (0xBD00_0000, 2),
            LdrD => (0xFD40_0000, 3),
            StrD => (0xFD00_0000, 3),
        }
    }

    pub fn size(self) -> u32 {
        1 << self.enc().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_immediates() {
        // Values checked against LLVM's encoder.
        assert_eq!(logical_imm(0xFF, 64), Some(0b1_000000_000111));
        assert_eq!(
            logical_imm(0x8000_0000_0000_0000, 64),
            Some(0b1_000001_000000)
        );
        assert_eq!(logical_imm(0x5555_5555, 32), Some(0b0_000000_111100));
        assert_eq!(logical_imm(0xFFFF_0000, 32), Some(0b0_010000_001111));
        assert_eq!(logical_imm(0, 32), None);
        assert_eq!(logical_imm(0xFFFF_FFFF, 32), None);
        assert_eq!(logical_imm(0x1234, 32), None);
    }
}

/// Cross-checks the encoder against the system assembler (clang). macOS only: it parses
/// the Mach-O object clang produces.
#[cfg(all(test, target_os = "macos"))]
mod assembler_check {
    use super::*;
    use std::process::Command;

    /// Extract the bytes of `__TEXT,__text` from a 64-bit Mach-O object.
    fn text_section(obj: &[u8]) -> Vec<u8> {
        let u32_at = |o: usize| u32::from_le_bytes(obj[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(obj[o..o + 8].try_into().unwrap());
        assert_eq!(u32_at(0), 0xFEED_FACF, "not a 64-bit Mach-O");
        let ncmds = u32_at(16) as usize;
        let mut off = 32;
        for _ in 0..ncmds {
            let (cmd, size) = (u32_at(off), u32_at(off + 4) as usize);
            if cmd == 0x19 {
                let nsects = u32_at(off + 64) as usize;
                for s in 0..nsects {
                    let so = off + 72 + 80 * s;
                    let name = std::str::from_utf8(&obj[so..so + 16])
                        .unwrap()
                        .trim_end_matches('\0');
                    if name == "__text" {
                        let sz = u64_at(so + 40) as usize;
                        let fo = u32_at(so + 48) as usize;
                        return obj[fo..fo + sz].to_vec();
                    }
                }
            }
            off += size;
        }
        panic!("no __text section");
    }

    #[test]
    fn encodings_match_clang() {
        let mut cases: Vec<(&str, Box<dyn Fn(&mut Asm)>)> = Vec::new();
        macro_rules! case {
            ($text:expr, |$a:ident| $e:expr) => {
                cases.push(($text, Box::new(|$a: &mut Asm| $e)));
            };
        }
        case!("add w1, w2, #4095", |a| a.add_imm(false, 1, 2, 4095));
        case!("add x3, sp, #16", |a| a.add_imm(true, 3, SP, 16));
        case!("add sp, x16, #0", |a| a.mov_sp(SP, 16));
        case!("sub x0, x1, #1, lsl #12", |a| a.sub_imm(true, 0, 1, 4096));
        case!("subs w5, w6, #7", |a| a.subs_imm(false, 5, 6, 7));
        case!("cmp x7, #100", |a| a.cmp_imm(true, 7, 100));
        case!("cmn w2, #1", |a| a.cmn_imm(false, 2, 1));
        case!("add x1, x2, x3", |a| a.add(true, 1, 2, 3));
        case!("adds w1, w2, w3", |a| a.adds(false, 1, 2, 3));
        case!("sub w4, w5, w6", |a| a.sub(false, 4, 5, 6));
        case!("cmp x16, x17", |a| a.cmp(true, 16, 17));
        case!("neg w0, w9", |a| a.neg(false, 0, 9));
        case!("add x17, sp, x16", |a| a.add_ext(17, SP, 16));
        case!("sub sp, sp, x16", |a| a.sub_ext(SP, SP, 16));
        case!("sub x16, sp, x17", |a| a.sub_ext(16, SP, 17));
        case!("add x16, x16, x5, lsl #2", |a| a
            .add_lsl(true, 16, 16, 5, 2));
        case!("and w1, w2, w3", |a| a.and(false, 1, 2, 3));
        case!("orr x1, xzr, x2", |a| a.mov(true, 1, 2));
        case!("eor x8, x9, x10", |a| a.eor(true, 8, 9, 10));
        case!("tst w3, w4", |a| a.tst(false, 3, 4));
        case!("and w0, w0, #0xff", |a| a.logical_imm(
            0,
            false,
            0,
            0,
            logical_imm(0xFF, 32).unwrap()
        ));
        case!("orr x1, x2, #0x8000000000000000", |a| a.logical_imm(
            1,
            true,
            1,
            2,
            logical_imm(0x8000_0000_0000_0000, 64).unwrap()
        ));
        case!("eor w3, w4, #0x55555555", |a| a.logical_imm(
            2,
            false,
            3,
            4,
            logical_imm(0x5555_5555, 32).unwrap()
        ));
        case!("udiv w1, w2, w3", |a| a.udiv(false, 1, 2, 3));
        case!("sdiv x1, x2, x3", |a| a.sdiv(true, 1, 2, 3));
        case!("lsl w1, w2, w3", |a| a.lslv(false, 1, 2, 3));
        case!("lsr x1, x2, x3", |a| a.lsrv(true, 1, 2, 3));
        case!("asr w1, w2, w3", |a| a.asrv(false, 1, 2, 3));
        case!("ror x1, x2, x3", |a| a.rorv(true, 1, 2, 3));
        case!("rbit w5, w6", |a| a.rbit(false, 5, 6));
        case!("clz x5, x6", |a| a.clz(true, 5, 6));
        case!("madd w1, w2, w3, w4", |a| a.madd(false, 1, 2, 3, 4));
        case!("msub x1, x2, x3, x4", |a| a.msub(true, 1, 2, 3, 4));
        case!("mul x1, x2, x3", |a| a.mul(true, 1, 2, 3));
        case!("lsl w1, w2, #5", |a| a.lsl_imm(false, 1, 2, 5));
        case!("lsl x1, x2, #63", |a| a.lsl_imm(true, 1, 2, 63));
        case!("lsr w1, w2, #31", |a| a.lsr_imm(false, 1, 2, 31));
        case!("lsr x26, x26, #16", |a| a.lsr_imm(true, 26, 26, 16));
        case!("asr x1, x2, #3", |a| a.asr_imm(true, 1, 2, 3));
        case!("ror w1, w2, #7", |a| a.ror_imm(false, 1, 2, 7));
        case!("ror x1, x2, #60", |a| a.ror_imm(true, 1, 2, 60));
        case!("sxtb w1, w2", |a| a.sxt(false, 1, 2, 8));
        case!("sxth x1, w2", |a| a.sxt(true, 1, 2, 16));
        case!("sxtw x1, w2", |a| a.sxt(true, 1, 2, 32));
        case!("uxtb w1, w2", |a| a.uxt(1, 2, 8));
        case!("uxth w1, w2", |a| a.uxt(1, 2, 16));
        case!("csel w1, w2, w3, ne", |a| a.csel(false, 1, 2, 3, Cond::Ne));
        case!("csel x1, x2, x3, lt", |a| a.csel(true, 1, 2, 3, Cond::Lt));
        case!("cset w1, hi", |a| a.cset(false, 1, Cond::Hi));
        case!("cset w16, ls", |a| a.cset(false, 16, Cond::Ls));
        case!("movz w1, #0x1234", |a| a.movz(false, 1, 0x1234, 0));
        case!("movz x1, #0x8000, lsl #48", |a| a.movz(true, 1, 0x8000, 48));
        case!("movk x1, #0xbeef, lsl #16", |a| a.movk(true, 1, 0xBEEF, 16));
        case!("movn w2, #0", |a| a.movn(false, 2, 0, 0));
        case!("ldr x1, [x2, #8]", |a| assert!(a.try_ldst(
            Mem::LdrX,
            1,
            2,
            8
        )));
        case!("ldr x1, [sp, #32760]", |a| assert!(a.try_ldst(
            Mem::LdrX,
            1,
            SP,
            32760
        )));
        case!("str x28, [sp, #24]", |a| assert!(a.try_ldst(
            Mem::StrX,
            28,
            SP,
            24
        )));
        case!("ldur x4, [x16, #-16]", |a| assert!(a.try_ldst(
            Mem::LdrX,
            4,
            16,
            -16
        )));
        case!("ldr w1, [x2, #4]", |a| assert!(a.try_ldst(
            Mem::LdrW,
            1,
            2,
            4
        )));
        case!("ldur w1, [x2, #3]", |a| assert!(a.try_ldst(
            Mem::LdrW,
            1,
            2,
            3
        )));
        case!("str w1, [x2, #4092]", |a| assert!(a.try_ldst(
            Mem::StrW,
            1,
            2,
            4092
        )));
        case!("ldrh w1, [x2, #2]", |a| assert!(a.try_ldst(
            Mem::LdrH,
            1,
            2,
            2
        )));
        case!("ldrsh w1, [x2]", |a| assert!(a.try_ldst(
            Mem::LdrShW,
            1,
            2,
            0
        )));
        case!("ldrsh x1, [x2]", |a| assert!(a.try_ldst(
            Mem::LdrShX,
            1,
            2,
            0
        )));
        case!("strh w1, [x2, #6]", |a| assert!(a.try_ldst(
            Mem::StrH,
            1,
            2,
            6
        )));
        case!("ldrb w1, [x2, #1]", |a| assert!(a.try_ldst(
            Mem::LdrB,
            1,
            2,
            1
        )));
        case!("ldrsb w1, [x2]", |a| assert!(a.try_ldst(
            Mem::LdrSbW,
            1,
            2,
            0
        )));
        case!("ldrsb x1, [x2]", |a| assert!(a.try_ldst(
            Mem::LdrSbX,
            1,
            2,
            0
        )));
        case!("strb w1, [x2, #4095]", |a| assert!(a.try_ldst(
            Mem::StrB,
            1,
            2,
            4095
        )));
        case!("ldrsw x1, [x2, #4]", |a| assert!(a.try_ldst(
            Mem::LdrSwX,
            1,
            2,
            4
        )));
        case!("ldr s1, [sp, #8]", |a| assert!(a.try_ldst(
            Mem::LdrS,
            1,
            SP,
            8
        )));
        case!("str s1, [x29, #16]", |a| assert!(a.try_ldst(
            Mem::StrS,
            1,
            FP,
            16
        )));
        case!("ldr d7, [x2, #24]", |a| assert!(a.try_ldst(
            Mem::LdrD,
            7,
            2,
            24
        )));
        case!("str d7, [sp, #40]", |a| assert!(a.try_ldst(
            Mem::StrD,
            7,
            SP,
            40
        )));
        case!("ldr w1, [x27, x2]", |a| a.ldst_regoff(
            Mem::LdrW,
            1,
            27,
            2,
            false
        ));
        case!("ldr x9, [x16, x10, lsl #3]", |a| a.ldst_regoff(
            Mem::LdrX,
            9,
            16,
            10,
            true
        ));
        case!("str x16, [sp, x17, lsl #3]", |a| a.ldst_regoff(
            Mem::StrX,
            16,
            SP,
            17,
            true
        ));
        case!("strb w3, [x27, x16]", |a| a.ldst_regoff(
            Mem::StrB,
            3,
            27,
            16,
            false
        ));
        case!("ldrsh x3, [x27, x16]", |a| a.ldst_regoff(
            Mem::LdrShX,
            3,
            27,
            16,
            false
        ));
        case!("ldr s3, [x27, x4]", |a| a.ldst_regoff(
            Mem::LdrS,
            3,
            27,
            4,
            false
        ));
        case!("str d3, [x27, x4]", |a| a.ldst_regoff(
            Mem::StrD,
            3,
            27,
            4,
            false
        ));
        case!("ldrsw x3, [x27, x4]", |a| a.ldst_regoff(
            Mem::LdrSwX,
            3,
            27,
            4,
            false
        ));
        case!("stp x29, x30, [sp, #-16]!", |a| a.stp_pre(FP, LR, SP, -16));
        case!("ldp x29, x30, [sp], #16", |a| a.ldp_post(FP, LR, SP, 16));
        case!("stp xzr, xzr, [sp, #48]", |a| a.stp(ZR, ZR, SP, 48));
        case!("ldp x1, x2, [sp, #-512]", |a| a.ldp(1, 2, SP, -512));
        case!("stp d8, d9, [sp, #-16]!", |a| a.stp_d_pre(8, 9, SP, -16));
        case!("ldp d14, d15, [sp], #16", |a| a.ldp_d_post(14, 15, SP, 16));
        case!("stp d10, d11, [sp, #32]", |a| a.stp_d(10, 11, SP, 32));
        case!("ldp d10, d11, [sp, #32]", |a| a.ldp_d(10, 11, SP, 32));
        case!("br x16", |a| a.br(16));
        case!("blr x1", |a| a.blr(1));
        case!("ret", |a| a.ret());
        case!("brk #0x1", |a| a.brk(1));
        case!("fmov s1, s2", |a| a.fmov(false, 1, 2));
        case!("fmov d1, d2", |a| a.fmov(true, 1, 2));
        case!("fabs s3, s4", |a| a.fabs(false, 3, 4));
        case!("fneg d3, d4", |a| a.fneg(true, 3, 4));
        case!("fsqrt s5, s6", |a| a.fsqrt(false, 5, 6));
        case!("fsqrt d5, d6", |a| a.fsqrt(true, 5, 6));
        case!("fcvt d1, s2", |a| a.fcvt(true, 1, 2));
        case!("fcvt s1, d2", |a| a.fcvt(false, 1, 2));
        case!("frintn s1, s2", |a| a.frintn(false, 1, 2));
        case!("frintp d1, d2", |a| a.frintp(true, 1, 2));
        case!("frintm s1, s2", |a| a.frintm(false, 1, 2));
        case!("frintz d1, d2", |a| a.frintz(true, 1, 2));
        case!("fmul s1, s2, s3", |a| a.fp2(false, 0, 1, 2, 3));
        case!("fdiv d1, d2, d3", |a| a.fp2(true, 1, 1, 2, 3));
        case!("fadd s1, s2, s3", |a| a.fp2(false, 2, 1, 2, 3));
        case!("fsub d1, d2, d3", |a| a.fp2(true, 3, 1, 2, 3));
        case!("fmax s1, s2, s3", |a| a.fp2(false, 4, 1, 2, 3));
        case!("fmin d1, d2, d3", |a| a.fp2(true, 5, 1, 2, 3));
        case!("fcmp s1, s2", |a| a.fcmp(false, 1, 2));
        case!("fcmp d29, d31", |a| a.fcmp(true, 29, 31));
        case!("fcsel s1, s2, s3, ne", |a| a.fcsel(
            false,
            1,
            2,
            3,
            Cond::Ne
        ));
        case!("fcsel d1, d2, d3, ge", |a| a.fcsel(true, 1, 2, 3, Cond::Ge));
        case!("scvtf s1, w2", |a| a.scvtf(false, false, 1, 2));
        case!("scvtf d1, x2", |a| a.scvtf(true, true, 1, 2));
        case!("ucvtf d1, w2", |a| a.ucvtf(false, true, 1, 2));
        case!("ucvtf s1, x2", |a| a.ucvtf(true, false, 1, 2));
        case!("fcvtzs w1, s2", |a| a.fcvtzs(false, false, 1, 2));
        case!("fcvtzs x1, d2", |a| a.fcvtzs(true, true, 1, 2));
        case!("fcvtzu w1, d2", |a| a.fcvtzu(false, true, 1, 2));
        case!("fcvtzu x1, s2", |a| a.fcvtzu(true, false, 1, 2));
        case!("fmov w1, s2", |a| a.fmov_to_gpr(false, 1, 2));
        case!("fmov x1, d2", |a| a.fmov_to_gpr(true, 1, 2));
        case!("fmov s1, w2", |a| a.fmov_from_gpr(false, 1, 2));
        case!("fmov d1, x2", |a| a.fmov_from_gpr(true, 1, 2));
        case!("fmov s1, wzr", |a| a.fmov_from_gpr(false, 1, ZR));
        case!("cnt v31.8b, v31.8b", |a| a.cnt8b(31, 31));
        case!("addv b31, v31.8b", |a| a.addv8b(31, 31));
        case!("bit v1.8b, v2.8b, v31.8b", |a| a.bit8b(1, 2, 31));
        case!("adr x16, #12", |a| a.adr(16, 12));
        // Label-relative branches: target 2 instructions ahead / 1 behind.
        case!("b #8", |a| {
            let l = a.new_label();
            a.b(l);
            a.emit(0xD503_201F);
            a.bind(l);
            a.finish().unwrap();
            a.code.pop();
        });
        case!("b.ne #-4", |a| {
            let l = a.new_label();
            a.bind(l);
            a.emit(0xD503_201F);
            a.b_cond(Cond::Ne, l);
            a.finish().unwrap();
            a.code.remove(0);
        });
        case!("cbz w3, #8", |a| {
            let l = a.new_label();
            a.cbz(false, 3, l);
            a.emit(0xD503_201F);
            a.bind(l);
            a.finish().unwrap();
            a.code.pop();
        });
        case!("cbnz x9, #8", |a| {
            let l = a.new_label();
            a.cbnz(true, 9, l);
            a.emit(0xD503_201F);
            a.bind(l);
            a.finish().unwrap();
            a.code.pop();
        });

        // mov_imm sequences, checked by value through the assembler's own expansion.
        let imm_cases: [(bool, u64); 8] = [
            (true, 0),
            (true, 0xFFFF_FFFF_FFFF_FFFF),
            (false, 0xFFFF_FFFF),
            (true, 0x1234_5678_9ABC_DEF0),
            (true, 0xFFFF_FFFF_FFFF_0001),
            (false, 0x8000_0000),
            (true, 0x0000_FFFF_0000_FFFF),
            (true, 0x7FF8_0000_0000_0000),
        ];

        let Ok(out) = Command::new("clang").arg("--version").output() else {
            eprintln!("clang not available; skipping");
            return;
        };
        assert!(out.status.success());
        let dir = std::env::temp_dir().join(format!("wisp-a64-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut src = String::from(".text\n");
        let mut ours: Vec<(String, Vec<u32>)> = Vec::new();
        for (text, f) in &cases {
            let mut a = Asm::new();
            f(&mut a);
            src.push_str(&format!("{text}\n"));
            ours.push((text.to_string(), a.code.clone()));
        }
        let s = dir.join("t.s");
        let o = dir.join("t.o");
        std::fs::write(&s, &src).unwrap();
        let st = Command::new("clang")
            .args(["-c", "-arch", "arm64", "-o"])
            .arg(&o)
            .arg(&s)
            .status()
            .unwrap();
        assert!(st.success(), "clang failed to assemble the test file");
        let text = text_section(&std::fs::read(&o).unwrap());
        let mut words = text
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()));
        let mut mismatches = Vec::new();
        for (t, ws) in &ours {
            for w in ws {
                let theirs = words.next().unwrap();
                if *w != theirs {
                    mismatches.push(format!("{t}: wisp {w:08x}, clang {theirs:08x}"));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "encoding mismatches:\n{}",
            mismatches.join("\n")
        );

        // mov_imm: execute nothing, but assemble our sequence and compare its value through
        // a disassembly-free check: emulate MOVZ/MOVN/MOVK/ORR to recover the constant.
        for (sf, v) in imm_cases {
            let mut a = Asm::new();
            a.mov_imm(sf, 5, v);
            let mut acc: u64 = 0;
            for &w in &a.code {
                let hw = (w >> 21) & 3;
                let imm = ((w >> 5) & 0xFFFF) as u64;
                match w & 0x7F80_0000 {
                    0x5280_0000 => acc = imm << (16 * hw),
                    0x1280_0000 => acc = !(imm << (16 * hw)),
                    0x7280_0000 => acc = (acc & !(0xFFFF << (16 * hw))) | (imm << (16 * hw)),
                    _ => panic!("unexpected instruction {w:08x} in mov_imm({v:#x})"),
                }
            }
            let want = if sf { v } else { v & 0xFFFF_FFFF };
            let got = if sf { acc } else { acc & 0xFFFF_FFFF };
            assert_eq!(got, want, "mov_imm({sf}, {v:#x})");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
