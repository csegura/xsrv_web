// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'l' => Some(Self::Little),
            b'B' => Some(Self::Big),
            _ => None,
        }
    }

    pub fn read_u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    pub fn read_u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn write_u16(self, value: u16) -> [u8; 2] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    fn write_u32(self, value: u32) -> [u8; 4] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }
}

pub fn padding(len: usize) -> usize {
    (4 - len % 4) % 4
}

pub fn padded(len: usize) -> usize {
    len + padding(len)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated;

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    endian: Endian,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], endian: Endian) -> Self {
        Self {
            buf,
            pos: 0,
            endian,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Truncated> {
        let end = self.pos.checked_add(len).ok_or(Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, Truncated> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Truncated> {
        let b = self.take(2)?;
        Ok(self.endian.read_u16([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Result<i16, Truncated> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32, Truncated> {
        let b = self.take(4)?;
        Ok(self.endian.read_u32([b[0], b[1], b[2], b[3]]))
    }

    pub fn skip(&mut self, len: usize) -> Result<(), Truncated> {
        self.take(len).map(|_| ())
    }

    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8], Truncated> {
        self.take(len)
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Fails when bytes are left. Requests have an exact size, so extra bytes are an error.
    pub fn end(&self) -> Result<(), Truncated> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(Truncated)
        }
    }
}

pub struct Writer {
    buf: Vec<u8>,
    endian: Endian,
}

impl Writer {
    pub fn new(endian: Endian) -> Self {
        Self {
            buf: Vec::new(),
            endian,
        }
    }

    pub fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&self.endian.write_u16(value));
    }

    pub fn i16(&mut self, value: i16) {
        self.u16(value as u16);
    }

    pub fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&self.endian.write_u32(value));
    }

    pub fn zeros(&mut self, len: usize) {
        self.buf.resize(self.buf.len() + len, 0);
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn pad(&mut self) {
        self.zeros(padding(self.buf.len()));
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_both_byte_orders() {
        let bytes = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(Reader::new(&bytes, Endian::Little).u32(), Ok(0x7856_3412));
        assert_eq!(Reader::new(&bytes, Endian::Big).u32(), Ok(0x1234_5678));
    }

    #[test]
    fn short_buffer_is_truncated() {
        let mut reader = Reader::new(&[1, 2, 3], Endian::Little);
        assert_eq!(reader.u32(), Err(Truncated));
        assert_eq!(reader.bytes(usize::MAX), Err(Truncated));
    }

    #[test]
    fn end_rejects_leftover_bytes() {
        let mut reader = Reader::new(&[1, 2], Endian::Little);
        assert_eq!(reader.u8(), Ok(1));
        assert_eq!(reader.end(), Err(Truncated));
    }

    #[test]
    fn writer_round_trips_and_pads() {
        let mut writer = Writer::new(Endian::Big);
        writer.u16(0xABCD);
        writer.u8(1);
        writer.pad();
        let bytes = writer.finish();
        assert_eq!(bytes, [0xAB, 0xCD, 1, 0]);
        assert_eq!(padded(5), 8);
    }
}
