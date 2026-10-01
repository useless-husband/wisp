//! A bounded byte reader with LEB128 decoding.
//!
//! Every read checks against `end`, which is the end of the innermost range being decoded
//! (a section, a function body, ...). Running past `end` is reported with the message the
//! reference interpreter uses for that context.

use crate::error::{Error, Result};
use crate::types::ValType;

#[derive(Clone)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub end: usize,
    /// Message used when a read runs past `end`.
    eof_msg: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader {
            data,
            pos: 0,
            end: data.len(),
            eof_msg: "unexpected end",
        }
    }

    /// A reader over `data[pos..end]`, positions stay absolute.
    pub fn sub(&self, pos: usize, end: usize, eof_msg: &'static str) -> Reader<'a> {
        Reader {
            data: self.data,
            pos,
            end,
            eof_msg,
        }
    }

    /// Allow reads up to the end of the whole input (the caller checks `end` afterwards).
    pub fn unbounded(&self) -> Reader<'a> {
        Reader {
            data: self.data,
            pos: self.pos,
            end: self.data.len(),
            eof_msg: self.eof_msg,
        }
    }

    pub fn eof(&self) -> bool {
        self.pos >= self.end
    }

    pub fn remaining(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }

    pub fn err<T>(&self, msg: impl Into<String>) -> Result<T> {
        Err(Error::malformed(self.pos, msg))
    }

    fn eof_err<T>(&self) -> Result<T> {
        Err(Error::malformed(self.pos, self.eof_msg))
    }

    pub fn u8(&mut self) -> Result<u8> {
        if self.pos >= self.end {
            return self.eof_err();
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    pub fn peek_u8(&self) -> Result<u8> {
        if self.pos >= self.end {
            return self.eof_err();
        }
        Ok(self.data[self.pos])
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return self.eof_err();
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u32_fixed(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64_fixed(&mut self) -> Result<u64> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    /// Next byte of an integer encoding. Like the reference interpreter, integers are read
    /// from the whole input and only then checked against the current bound, so an
    /// overlong encoding is reported as such even when it crosses a section end.
    fn leb_byte(&mut self) -> Result<u8> {
        if self.pos >= self.data.len() {
            return self.eof_err();
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// A value that ran past `end` is accepted here; the enclosing section's size check (or
    /// the next bounded read) reports it, which is the order the reference decoder uses.
    fn leb_done<T>(&mut self, v: T) -> Result<T> {
        Ok(v)
    }

    /// Unsigned LEB128 of at most `bits` bits.
    fn uleb(&mut self, bits: u32) -> Result<u64> {
        let max_bytes = bits.div_ceil(7);
        let mut result: u64 = 0;
        let mut shift = 0u32;
        for i in 0..max_bytes {
            let b = self.leb_byte()?;
            let payload = (b & 0x7F) as u64;
            if i == max_bytes - 1 {
                // Last permitted byte: the continuation bit must be clear and unused bits zero.
                if b & 0x80 != 0 {
                    return Err(Error::malformed(
                        self.pos - 1,
                        "integer representation too long",
                    ));
                }
                let used = bits - shift;
                if payload >> used != 0 {
                    return Err(Error::malformed(self.pos - 1, "integer too large"));
                }
            }
            result |= payload << shift;
            if b & 0x80 == 0 {
                return self.leb_done(result);
            }
            shift += 7;
        }
        unreachable!()
    }

    /// Signed LEB128 of at most `bits` bits.
    fn sleb(&mut self, bits: u32) -> Result<i64> {
        let max_bytes = bits.div_ceil(7);
        let mut result: i64 = 0;
        let mut shift = 0u32;
        for i in 0..max_bytes {
            let b = self.leb_byte()?;
            let payload = (b & 0x7F) as i64;
            if i == max_bytes - 1 {
                if b & 0x80 != 0 {
                    return Err(Error::malformed(
                        self.pos - 1,
                        "integer representation too long",
                    ));
                }
                // The unused high bits must be a sign extension of the last used bit.
                let used = bits - shift; // number of value bits in this byte
                let sign_and_unused = payload >> (used - 1); // bits [used-1, 7)
                let all = (1i64 << (7 - used + 1)) - 1;
                if sign_and_unused != 0 && sign_and_unused != all {
                    return Err(Error::malformed(self.pos - 1, "integer too large"));
                }
            }
            result |= payload << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && (b & 0x40) != 0 {
                    result |= -1i64 << shift;
                }
                return self.leb_done(result);
            }
        }
        unreachable!()
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(self.uleb(32)? as u32)
    }

    pub fn u64(&mut self) -> Result<u64> {
        self.uleb(64)
    }

    pub fn s32(&mut self) -> Result<i32> {
        Ok(self.sleb(32)? as i32)
    }

    pub fn s33(&mut self) -> Result<i64> {
        self.sleb(33)
    }

    pub fn s64(&mut self) -> Result<i64> {
        self.sleb(64)
    }

    /// A length-prefixed UTF-8 name.
    pub fn name(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        if self.pos > self.end {
            return self.eof_err();
        }
        if len > self.remaining() {
            return Err(Error::malformed(self.pos, "length out of bounds"));
        }
        let start = self.pos;
        let b = self.bytes(len)?;
        match std::str::from_utf8(b) {
            Ok(s) => Ok(s.to_string()),
            Err(_) => Err(Error::malformed(start, "malformed UTF-8 encoding")),
        }
    }

    pub fn val_type(&mut self) -> Result<ValType> {
        let b = self.u8()?;
        match ValType::from_byte(b) {
            Some(t) => Ok(t),
            None => Err(Error::malformed(
                self.pos - 1,
                format!("malformed value type 0x{b:02x}"),
            )),
        }
    }

    pub fn ref_type(&mut self) -> Result<ValType> {
        let b = self.u8()?;
        match b {
            0x70 => Ok(ValType::FuncRef),
            0x6F => Ok(ValType::ExternRef),
            _ => Err(Error::malformed(self.pos - 1, "malformed reference type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_of(b: &[u8]) -> Result<u32> {
        Reader::new(b).u32()
    }
    fn s32_of(b: &[u8]) -> Result<i32> {
        Reader::new(b).s32()
    }
    fn s64_of(b: &[u8]) -> Result<i64> {
        Reader::new(b).s64()
    }

    #[test]
    fn unsigned() {
        assert_eq!(u32_of(&[0x00]).unwrap(), 0);
        assert_eq!(u32_of(&[0xE5, 0x8E, 0x26]).unwrap(), 624485);
        assert_eq!(u32_of(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).unwrap(), u32::MAX);
        // Non-minimal encodings are allowed.
        assert_eq!(u32_of(&[0x80, 0x80, 0x80, 0x80, 0x00]).unwrap(), 0);
        assert_eq!(
            u32_of(&[0xFF, 0xFF, 0xFF, 0xFF, 0x1F])
                .unwrap_err()
                .message(),
            "integer too large"
        );
        assert_eq!(
            u32_of(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00])
                .unwrap_err()
                .message(),
            "integer representation too long"
        );
        assert_eq!(u32_of(&[0x80]).unwrap_err().message(), "unexpected end");
    }

    #[test]
    fn signed() {
        assert_eq!(s32_of(&[0x7F]).unwrap(), -1);
        assert_eq!(s32_of(&[0xC0, 0xBB, 0x78]).unwrap(), -123456);
        assert_eq!(s32_of(&[0x80, 0x80, 0x80, 0x80, 0x78]).unwrap(), i32::MIN);
        assert_eq!(s32_of(&[0xFF, 0xFF, 0xFF, 0xFF, 0x07]).unwrap(), i32::MAX);
        assert_eq!(
            s32_of(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F])
                .unwrap_err()
                .message(),
            "integer too large"
        );
        assert_eq!(
            s32_of(&[0x80, 0x80, 0x80, 0x80, 0x70])
                .unwrap_err()
                .message(),
            "integer too large"
        );
        assert_eq!(
            s64_of(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7F]).unwrap(),
            i64::MIN
        );
        assert_eq!(
            s64_of(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]).unwrap(),
            i64::MAX
        );
        assert_eq!(
            s64_of(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x41])
                .unwrap_err()
                .message(),
            "integer too large"
        );
        // s33 block types.
        assert_eq!(Reader::new(&[0x40]).s33().unwrap(), -64);
    }

    #[test]
    fn utf8_names() {
        assert_eq!(Reader::new(&[2, b'h', b'i']).name().unwrap(), "hi");
        assert_eq!(
            Reader::new(&[2, 0xC0, 0x80]).name().unwrap_err().message(),
            "malformed UTF-8 encoding"
        );
        assert_eq!(
            Reader::new(&[5, b'a']).name().unwrap_err().message(),
            "length out of bounds"
        );
    }
}
