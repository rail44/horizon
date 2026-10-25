//! Protobuf wire format: varints, field keys, and skipping by wire type.

use std::io::{self, Read};

use crate::{Error, Result};

pub(crate) const VARINT: u8 = 0;
pub(crate) const FIXED64: u8 = 1;
pub(crate) const LEN: u8 = 2;
pub(crate) const FIXED32: u8 = 5;

/// One field of a message held in memory.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Value<'a> {
    Varint(u64),
    Len(&'a [u8]),
    /// A fixed32 or fixed64 field; no field read here has these types.
    Fixed,
}

/// Iterates over the fields of a message held in memory.
pub(crate) struct Fields<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64> {
        let (value, used) = decode_varint(&self.data[self.pos..])?;
        self.pos += used;
        Ok(value)
    }

    fn take(&mut self, len: u64) -> Result<&'a [u8]> {
        let end = usize::try_from(len)
            .ok()
            .and_then(|len| self.pos.checked_add(len))
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| Error::new("truncated message: a field runs past its end"))?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u32, Value<'a>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        Some((|| {
            let (number, wire) = split_key(self.varint()?)?;
            let value = match wire {
                VARINT => Value::Varint(self.varint()?),
                LEN => {
                    let len = self.varint()?;
                    Value::Len(self.take(len)?)
                }
                FIXED64 => {
                    self.take(8)?;
                    Value::Fixed
                }
                FIXED32 => {
                    self.take(4)?;
                    Value::Fixed
                }
                _ => unreachable!("split_key rejects other wire types"),
            };
            Ok((number, value))
        })())
    }
}

/// Splits a field key into its number and wire type; groups (3, 4) and
/// undefined wire types are errors, as they never occur in proto3.
pub(crate) fn split_key(key: u64) -> Result<(u32, u8)> {
    let wire = (key & 7) as u8;
    let number = u32::try_from(key >> 3).map_err(|_| Error::new("field number out of range"))?;
    if number == 0 {
        return Err(Error::new("field number 0"));
    }
    match wire {
        VARINT | FIXED64 | LEN | FIXED32 => Ok((number, wire)),
        _ => Err(Error::new(format!("unsupported wire type {wire}"))),
    }
}

/// Decodes a varint at the start of `data`, returning it and its length.
pub(crate) fn decode_varint(data: &[u8]) -> Result<(u64, usize)> {
    let mut value = 0u64;
    for (index, &byte) in data.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte < 0x80 {
            return Ok((value, index + 1));
        }
    }
    if data.len() < 10 {
        Err(Error::new("truncated varint"))
    } else {
        Err(Error::new("varint longer than ten bytes"))
    }
}

/// Reads a varint from a stream; `None` when the stream ends before its first byte.
pub(crate) fn read_varint(input: &mut impl Read) -> Result<Option<u64>> {
    let mut value = 0u64;
    for index in 0..10 {
        let mut byte = [0u8];
        if let Err(error) = input.read_exact(&mut byte) {
            return if error.kind() == io::ErrorKind::UnexpectedEof {
                if index == 0 {
                    Ok(None)
                } else {
                    Err(Error::new("truncated varint"))
                }
            } else {
                Err(error.into())
            };
        }
        value |= u64::from(byte[0] & 0x7f) << (7 * index);
        if byte[0] < 0x80 {
            return Ok(Some(value));
        }
    }
    Err(Error::new("varint longer than ten bytes"))
}

/// An `int32` field: protobuf encodes negative values as ten-byte varints of
/// the sign-extended value, so the low 32 bits are the value.
pub(crate) fn int32(value: u64) -> i32 {
    value as u32 as i32
}

/// The elements of a `repeated int32` field occurrence, packed (one
/// length-delimited field) or not (one varint per occurrence).
pub(crate) fn push_int32s(out: &mut Vec<i32>, value: Value<'_>) -> Result<()> {
    match value {
        Value::Varint(value) => out.push(int32(value)),
        Value::Len(mut data) => {
            while !data.is_empty() {
                let (value, used) = decode_varint(data)?;
                out.push(int32(value));
                data = &data[used..];
            }
        }
        Value::Fixed => {}
    }
    Ok(())
}

pub(crate) fn string(data: &[u8]) -> Result<String> {
    String::from_utf8(data.to_vec()).map_err(|_| Error::new("string field is not valid UTF-8"))
}
