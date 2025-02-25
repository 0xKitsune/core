// Copyright 2015-2020 Parity Technologies
// Copyright 2023-2023 Alloy Contributors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
//

use crate::{encode, token::TokenSeq, utils, Error, Result, TokenType, Word};
use alloc::{borrow::Cow, vec::Vec};
use core::{fmt, slice::SliceIndex};

/// The [`Decoder`] wraps a byte slice with necessary info to progressively
/// deserialize the bytes into a sequence of tokens.
///
/// # Usage Note
///
/// While the Decoder contains the necessary info, the actual deserialization
/// is done in the [`crate::SolType`] trait.
#[derive(Clone, Copy)]
pub struct Decoder<'de> {
    // the underlying buffer
    buf: &'de [u8],
    // the current offset in the buffer
    offset: usize,
    // true if we validate type correctness and blob re-encoding
    validate: bool,
}

impl fmt::Debug for Decoder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut body = self
            .buf
            .chunks(32)
            .map(hex::encode_prefixed)
            .collect::<Vec<_>>();
        body[self.offset / 32].push_str(" <-- Next Word");

        f.debug_struct("Decoder")
            .field("buf", &body)
            .field("offset", &self.offset)
            .field("validate", &self.validate)
            .finish()
    }
}

impl fmt::Display for Decoder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Abi Decode Buffer")?;

        for (i, chunk) in self.buf.chunks(32).enumerate() {
            writeln!(
                f,
                "0x{:04x}: {} {}",
                i * 32,
                hex::encode_prefixed(chunk),
                if i * 32 == self.offset {
                    " <-- Next Word"
                } else {
                    ""
                }
            )?;
        }
        Ok(())
    }
}

impl<'de> Decoder<'de> {
    /// Instantiate a new decoder from a byte slice and a validation flag.
    ///
    /// If `validate` is true, the decoder will check that the bytes conform to
    /// expected type limitations, and that the decoded values can be re-encoded
    /// to an identical bytestring.
    #[inline]
    pub const fn new(buf: &'de [u8], validate: bool) -> Self {
        Self {
            buf,
            offset: 0,
            validate,
        }
    }

    /// Create a child decoder, starting at `offset` bytes from the current
    /// decoder's offset. The child decoder shares the buffer and validation
    /// flag.
    #[inline]
    fn child(&self, offset: usize) -> Result<Decoder<'de>, Error> {
        self.buf
            .get(offset..)
            .map(|buf| Self {
                buf,
                offset: 0,
                validate: self.validate,
            })
            .ok_or(Error::Overrun)
    }

    /// Get a child decoder at the current offset.
    #[inline]
    pub fn raw_child(&self) -> Decoder<'de> {
        self.child(self.offset).unwrap()
    }

    /// Advance the offset by `len` bytes.
    #[inline]
    fn increase_offset(&mut self, len: usize) {
        self.offset += len;
    }

    /// Peek into the buffer.
    #[inline]
    pub fn peek<I: SliceIndex<[u8]>>(&self, index: I) -> Result<&'de I::Output, Error> {
        self.buf.get(index).ok_or(Error::Overrun)
    }

    /// Peek a slice of size `len` from the buffer at a specific offset, without
    /// advancing the offset.
    #[inline]
    pub fn peek_len_at(&self, offset: usize, len: usize) -> Result<&'de [u8], Error> {
        self.peek(offset..offset + len)
    }

    /// Peek a slice of size `len` from the buffer without advancing the offset.
    #[inline]
    pub fn peek_len(&self, len: usize) -> Result<&'de [u8], Error> {
        self.peek_len_at(self.offset, len)
    }

    /// Peek a word from the buffer at a specific offset, without advancing the
    /// offset.
    #[inline]
    pub fn peek_word_at(&self, offset: usize) -> Result<Word, Error> {
        Ok(Word::from_slice(
            self.peek_len_at(offset, Word::len_bytes())?,
        ))
    }

    /// Peek the next word from the buffer without advancing the offset.
    #[inline]
    pub fn peek_word(&self) -> Result<Word, Error> {
        self.peek_word_at(self.offset)
    }

    /// Peek a u32 from the buffer at a specific offset, without advancing the
    /// offset.
    #[inline]
    pub fn peek_u32_at(&self, offset: usize) -> Result<u32> {
        utils::as_u32(self.peek_word_at(offset)?, true)
    }

    /// Peek the next word as a u32.
    #[inline]
    pub fn peek_u32(&self) -> Result<u32> {
        utils::as_u32(self.peek_word()?, true)
    }

    /// Take a word from the buffer, advancing the offset.
    #[inline]
    pub fn take_word(&mut self) -> Result<Word, Error> {
        let contents = self.peek_word()?;
        self.increase_offset(Word::len_bytes());
        Ok(contents)
    }

    /// Return a child decoder by consuming a word, interpreting it as a
    /// pointer, and following it.
    #[inline]
    pub fn take_indirection(&mut self) -> Result<Decoder<'de>, Error> {
        let ptr = self.take_u32()? as usize;
        self.child(ptr)
    }

    /// Take a u32 from the buffer by consuming a word.
    #[inline]
    pub fn take_u32(&mut self) -> Result<u32> {
        let word = self.take_word()?;
        utils::as_u32(word, true)
    }

    /// Takes a slice of bytes of the given length by consuming up to the next
    /// word boundary.
    pub fn take_slice(&mut self, len: usize) -> Result<&[u8], Error> {
        if self.validate {
            let padded_len = utils::next_multiple_of_32(len);
            if self.offset + padded_len > self.buf.len() {
                return Err(Error::Overrun)
            }
            if !utils::check_zeroes(self.peek(self.offset + len..self.offset + padded_len)?) {
                return Err(Error::Other(Cow::Borrowed(
                    "Non-empty bytes after packed array",
                )))
            }
        }
        let res = self.peek_len(len)?;
        self.increase_offset(len);
        Ok(res)
    }

    /// True if this decoder is validating type correctness.
    #[inline]
    pub const fn validate(&self) -> bool {
        self.validate
    }

    /// Takes the offset from the child decoder and sets it as the current
    /// offset.
    #[inline]
    pub fn take_offset(&mut self, child: Decoder<'de>) {
        self.set_offset(child.offset + (self.buf.len() - child.buf.len()))
    }

    /// Sets the current offset in the buffer.
    #[inline]
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    /// Returns the current offset in the buffer.
    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Decodes a single token from the underlying buffer.
    #[inline]
    pub fn decode<T: TokenType<'de>>(&mut self) -> Result<T> {
        T::decode_from(self)
    }

    /// Decodes a sequence of tokens from the underlying buffer.
    #[inline]
    pub fn decode_sequence<T: TokenType<'de> + TokenSeq<'de>>(&mut self) -> Result<T> {
        T::decode_sequence(self)
    }
}

/// Decodes ABI compliant vector of bytes into vector of tokens described by
/// types param.
pub fn decode<'de, T: TokenSeq<'de>>(data: &'de [u8], validate: bool) -> Result<T> {
    let mut decoder = Decoder::new(data, validate);
    let res = decoder.decode_sequence::<T>()?;
    if validate && encode(&res) != data {
        return Err(Error::ReserMismatch)
    }
    Ok(res)
}

/// Decode a single token.
#[inline]
pub fn decode_single<'de, T: TokenType<'de>>(data: &'de [u8], validate: bool) -> Result<T> {
    decode::<(T,)>(data, validate).map(|(t,)| t)
}

/// Decode top-level function args. Encodes as params if T is a tuple.
/// Otherwise, wraps in a tuple and decodes.
#[inline]
pub fn decode_params<'de, T: TokenSeq<'de>>(data: &'de [u8], validate: bool) -> Result<T> {
    if T::IS_TUPLE {
        decode(data, validate)
    } else {
        decode_single(data, validate)
    }
}

#[cfg(test)]
mod tests {
    use crate::{sol_data, utils::pad_u32, SolType};
    use alloc::string::ToString;
    use alloy_primitives::{Address, B256, U256};
    use hex_literal::hex;

    #[test]
    fn dynamic_array_of_dynamic_arrays() {
        type MyTy = sol_data::Array<sol_data::Array<sol_data::Address>>;
        let encoded = hex!(
            "
    		0000000000000000000000000000000000000000000000000000000000000020
    		0000000000000000000000000000000000000000000000000000000000000002
    		0000000000000000000000000000000000000000000000000000000000000040
    		0000000000000000000000000000000000000000000000000000000000000080
    		0000000000000000000000000000000000000000000000000000000000000001
    		0000000000000000000000001111111111111111111111111111111111111111
    		0000000000000000000000000000000000000000000000000000000000000001
    		0000000000000000000000002222222222222222222222222222222222222222
    	"
        );

        assert_eq!(
            MyTy::encode_params(&vec![
                vec![Address::repeat_byte(0x11)],
                vec![Address::repeat_byte(0x22)],
            ]),
            encoded
        );

        MyTy::decode_params(&encoded, false).unwrap();
    }

    #[test]
    fn decode_static_tuple_of_addresses_and_uints() {
        type MyTy = (sol_data::Address, sol_data::Address, sol_data::Uint<256>);

        let encoded = hex!(
            "
    		0000000000000000000000001111111111111111111111111111111111111111
    		0000000000000000000000002222222222222222222222222222222222222222
    		1111111111111111111111111111111111111111111111111111111111111111
    	"
        );
        let address1 = Address::from([0x11u8; 20]);
        let address2 = Address::from([0x22u8; 20]);
        let uint = U256::from_be_bytes::<32>([0x11u8; 32]);
        let expected = (address1, address2, uint);
        let decoded = MyTy::decode(&encoded, true).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_dynamic_tuple() {
        type MyTy = (sol_data::String, sol_data::String);
        let encoded = hex!(
            "
    		0000000000000000000000000000000000000000000000000000000000000020
    		0000000000000000000000000000000000000000000000000000000000000040
    		0000000000000000000000000000000000000000000000000000000000000080
    		0000000000000000000000000000000000000000000000000000000000000009
    		6761766f66796f726b0000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000009
    		6761766f66796f726b0000000000000000000000000000000000000000000000
    	"
        );
        let string1 = "gavofyork".to_string();
        let string2 = "gavofyork".to_string();
        let expected = (string1, string2);

        // this test vector contains a top-level indirect
        let decoded = MyTy::decode_single(&encoded, true).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_nested_tuple() {
        type MyTy = (
            sol_data::String,
            sol_data::Bool,
            sol_data::String,
            (
                sol_data::String,
                sol_data::String,
                (sol_data::String, sol_data::String),
            ),
        );

        let encoded = hex!(
            "
    		0000000000000000000000000000000000000000000000000000000000000020
    		0000000000000000000000000000000000000000000000000000000000000080
    		0000000000000000000000000000000000000000000000000000000000000001
    		00000000000000000000000000000000000000000000000000000000000000c0
    		0000000000000000000000000000000000000000000000000000000000000100
    		0000000000000000000000000000000000000000000000000000000000000004
    		7465737400000000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000006
    		6379626f72670000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000060
    		00000000000000000000000000000000000000000000000000000000000000a0
    		00000000000000000000000000000000000000000000000000000000000000e0
    		0000000000000000000000000000000000000000000000000000000000000005
    		6e69676874000000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000003
    		6461790000000000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000040
    		0000000000000000000000000000000000000000000000000000000000000080
    		0000000000000000000000000000000000000000000000000000000000000004
    		7765656500000000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000008
    		66756e7465737473000000000000000000000000000000000000000000000000
    	"
        );
        let string1 = "test".into();
        let string2 = "cyborg".into();
        let string3 = "night".into();
        let string4 = "day".into();
        let string5 = "weee".into();
        let string6 = "funtests".into();
        let bool = true;
        let deep_tuple = (string5, string6);
        let inner_tuple = (string3, string4, deep_tuple);
        let expected = (string1, bool, string2, inner_tuple);

        let decoded = MyTy::decode_single(&encoded, true).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_complex_tuple_of_dynamic_and_static_types() {
        type MyTy = (
            sol_data::Uint<256>,
            sol_data::String,
            sol_data::Address,
            sol_data::Address,
        );

        let encoded = hex!(
            "
    		0000000000000000000000000000000000000000000000000000000000000020
    		1111111111111111111111111111111111111111111111111111111111111111
    		0000000000000000000000000000000000000000000000000000000000000080
    		0000000000000000000000001111111111111111111111111111111111111111
    		0000000000000000000000002222222222222222222222222222222222222222
    		0000000000000000000000000000000000000000000000000000000000000009
    		6761766f66796f726b0000000000000000000000000000000000000000000000
    	"
        );
        let uint = U256::from_be_bytes::<32>([0x11u8; 32]);
        let string = "gavofyork".to_string();
        let address1 = Address::from([0x11u8; 20]);
        let address2 = Address::from([0x22u8; 20]);
        let expected = (uint, string, address1, address2);

        let decoded = MyTy::decode_single(&encoded, true).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_params_containing_dynamic_tuple() {
        type MyTy = (
            sol_data::Address,
            (sol_data::Bool, sol_data::String, sol_data::String),
            sol_data::Address,
            sol_data::Address,
            sol_data::Bool,
        );

        let encoded = hex!(
            "
    		0000000000000000000000002222222222222222222222222222222222222222
    		00000000000000000000000000000000000000000000000000000000000000a0
    		0000000000000000000000003333333333333333333333333333333333333333
    		0000000000000000000000004444444444444444444444444444444444444444
    		0000000000000000000000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000001
    		0000000000000000000000000000000000000000000000000000000000000060
    		00000000000000000000000000000000000000000000000000000000000000a0
    		0000000000000000000000000000000000000000000000000000000000000009
    		7370616365736869700000000000000000000000000000000000000000000000
    		0000000000000000000000000000000000000000000000000000000000000006
    		6379626f72670000000000000000000000000000000000000000000000000000
    	"
        );
        let address1 = Address::from([0x22u8; 20]);
        let bool1 = true;
        let string1 = "spaceship".to_string();
        let string2 = "cyborg".to_string();
        let tuple = (bool1, string1, string2);
        let address2 = Address::from([0x33u8; 20]);
        let address3 = Address::from([0x44u8; 20]);
        let bool2 = false;
        let expected = (address1, tuple, address2, address3, bool2);

        let decoded = MyTy::decode_params(&encoded, true).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_params_containing_static_tuple() {
        type MyTy = (
            sol_data::Address,
            (sol_data::Address, sol_data::Bool, sol_data::Bool),
            sol_data::Address,
            sol_data::Address,
        );

        let encoded = hex!(
            "
    		0000000000000000000000001111111111111111111111111111111111111111
    		0000000000000000000000002222222222222222222222222222222222222222
    		0000000000000000000000000000000000000000000000000000000000000001
    		0000000000000000000000000000000000000000000000000000000000000000
    		0000000000000000000000003333333333333333333333333333333333333333
    		0000000000000000000000004444444444444444444444444444444444444444
    	"
        );
        let address1 = Address::from([0x11u8; 20]);
        let address2 = Address::from([0x22u8; 20]);
        let bool1 = true;
        let bool2 = false;
        let tuple = (address2, bool1, bool2);
        let address3 = Address::from([0x33u8; 20]);
        let address4 = Address::from([0x44u8; 20]);

        let expected = (address1, tuple, address3, address4);

        let decoded = MyTy::decode_params(&encoded, false).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_data_with_size_that_is_not_a_multiple_of_32() {
        type MyTy = (
            sol_data::Uint<256>,
            sol_data::String,
            sol_data::String,
            sol_data::Uint<256>,
            sol_data::Uint<256>,
        );

        let data = (
            pad_u32(0).into(),
            "12203967b532a0c14c980b5aeffb17048bdfaef2c293a9509f08eb3c6b0f5f8f0942e7b9cc76ca51cca26ce546920448e308fda6870b5e2ae12a2409d942de428113P720p30fps16x9".to_string(),
            "93c717e7c0a6517a".to_string(),
            pad_u32(1).into(),
            pad_u32(5538829).into()
        );

        let encoded = hex!(
            "
            0000000000000000000000000000000000000000000000000000000000000000
            00000000000000000000000000000000000000000000000000000000000000a0
            0000000000000000000000000000000000000000000000000000000000000152
            0000000000000000000000000000000000000000000000000000000000000001
            000000000000000000000000000000000000000000000000000000000054840d
            0000000000000000000000000000000000000000000000000000000000000092
            3132323033393637623533326130633134633938306235616566666231373034
            3862646661656632633239336139353039663038656233633662306635663866
            3039343265376239636337366361353163636132366365353436393230343438
            6533303866646136383730623565326165313261323430396439343264653432
            3831313350373230703330667073313678390000000000000000000000000000
            0000000000000000000000000000000000103933633731376537633061363531
            3761
        "
        );

        assert_eq!(MyTy::decode(&encoded, false).unwrap(), data,);
    }

    #[test]
    fn decode_after_fixed_bytes_with_less_than_32_bytes() {
        type MyTy = (
            sol_data::Address,
            sol_data::FixedBytes<32>,
            sol_data::FixedBytes<4>,
            sol_data::String,
        );

        let encoded = hex!(
            "
    		0000000000000000000000008497afefdc5ac170a664a231f6efb25526ef813f
    		0101010101010101010101010101010101010101010101010101010101010101
    		0202020202020202020202020202020202020202020202020202020202020202
    		0000000000000000000000000000000000000000000000000000000000000080
    		000000000000000000000000000000000000000000000000000000000000000a
    		3078303030303030314600000000000000000000000000000000000000000000
    	    "
        );

        assert_eq!(
            MyTy::decode_params(&encoded, false).unwrap(),
            (
                hex!("8497afefdc5ac170a664a231f6efb25526ef813f").into(),
                B256::repeat_byte(0x01).into(),
                [0x02; 4],
                "0x0000001F".into(),
            )
        );
    }

    #[test]
    fn decode_broken_utf8() {
        let encoded = hex!(
            "
    		0000000000000000000000000000000000000000000000000000000000000020
    		0000000000000000000000000000000000000000000000000000000000000004
    		e4b88de500000000000000000000000000000000000000000000000000000000
            "
        );

        assert_eq!(
            sol_data::String::decode_single(&encoded, false).unwrap(),
            "不�".to_string()
        );
    }

    #[test]
    fn decode_corrupted_dynamic_array() {
        type MyTy = sol_data::Array<sol_data::Uint<32>>;
        // line 1 at 0x00 =   0: tail offset of array
        // line 2 at 0x20 =  32: length of array
        // line 3 at 0x40 =  64: first word
        // line 4 at 0x60 =  96: second word
        let encoded = hex!(
            "
    	0000000000000000000000000000000000000000000000000000000000000020
    	00000000000000000000000000000000000000000000000000000000ffffffff
    	0000000000000000000000000000000000000000000000000000000000000001
    	0000000000000000000000000000000000000000000000000000000000000002
        "
        );
        assert!(MyTy::decode(&encoded, true).is_err());
    }

    #[test]
    fn decode_verify_addresses() {
        let input = hex!(
            "
    	0000000000000000000000000000000000000000000000000000000000012345
    	0000000000000000000000000000000000000000000000000000000000054321
    	"
        );
        assert!(sol_data::Address::decode_single(&input, false).is_ok());
        assert!(sol_data::Address::decode_single(&input, true).is_err());
        assert!(<(sol_data::Address, sol_data::Address)>::decode_single(&input, true).is_ok());
    }

    #[test]
    fn decode_verify_bytes() {
        type MyTy = (sol_data::Address, sol_data::FixedBytes<20>);
        type MyTy2 = (sol_data::Address, sol_data::Address);

        let input = hex!(
            "
    	0000000000000000000000001234500000000000000000000000000000012345
    	0000000000000000000000005432100000000000000000000000000000054321
    	"
        );
        MyTy::decode_params(&input, true).unwrap_err();
        assert!(MyTy2::decode_params(&input, true).is_ok());
    }

    #[test]
    fn signed_int_dirty_high_bytes() {
        type MyTy = sol_data::Int<8>;

        let dirty_negative =
            hex!("f0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");

        assert_eq!(MyTy::decode_single(&dirty_negative, false).unwrap(), -1);

        assert!(
            matches!(
                MyTy::decode_single(&dirty_negative, true),
                Err(crate::Error::TypeCheckFail { .. }),
            ),
            "did not match error"
        );

        let dirty_positive =
            hex!("700000000000000000000000000000000000000000000000000000000000007f");

        assert_eq!(MyTy::decode_single(&dirty_positive, false).unwrap(), 127);

        assert!(
            matches!(
                MyTy::decode_single(&dirty_positive, true),
                Err(crate::Error::TypeCheckFail { .. }),
            ),
            "did not match error"
        );
    }
}
