// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{take_n_padded, trace_header, ParseError, ParseResult, STRING_RECORD_TYPE};
use nom::combinator::all_consuming;
use std::num::NonZeroU16;

pub(crate) const STRING_REF_INLINE_BIT: u16 = 1 << 15;

#[derive(Clone, Debug, PartialEq)]
pub enum StringRef<'a> {
    Empty,
    Index(NonZeroU16),
    Inline(&'a str),
}

impl<'a> StringRef<'a> {
    pub(crate) fn parse(str_ref: u16, buf: &'a [u8]) -> ParseResult<'a, Self> {
        if let Some(nonzero) = NonZeroU16::new(str_ref) {
            if (nonzero.get() >> 15) & 1 == 0 {
                // MSB is zero, so this is a string index.
                Ok((buf, StringRef::Index(nonzero)))
            } else {
                // Remove the MSB from the length.
                let length = str_ref ^ STRING_REF_INLINE_BIT;
                let (buf, inline) = parse_padded_string(length as usize, buf)?;
                Ok((buf, StringRef::Inline(inline)))
            }
        } else {
            Ok((buf, StringRef::Empty))
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct StringRecord<'a> {
    /// Index should not be 0 but we can't use NonZeroU16 according to the spec:
    ///
    /// > String records that contain empty strings must be tolerated but they're pointless since
    /// > the empty string can simply be encoded as zero in a string ref.
    pub index: u16,
    pub value: &'a str,
}

impl<'a> StringRecord<'a> {
    pub(super) fn parse(buf: &'a [u8]) -> ParseResult<'a, Self> {
        let (buf, header) = StringHeader::parse(buf)?;
        let (rem, payload) = header.take_payload(buf)?;
        let (empty, value) =
            all_consuming(|p| parse_padded_string(header.string_len() as usize, p))(payload)?;
        assert!(empty.is_empty(), "all_consuming must not return any remaining buffer");
        Ok((rem, Self { index: header.string_index(), value }))
    }
}

trace_header! {
    StringHeader (STRING_RECORD_TYPE) {
        u16, string_index: 16, 30;
        u16, string_len: 32, 46;
    }
}

pub(crate) fn parse_padded_string<'a>(
    unpadded_len: usize,
    buf: &'a [u8],
) -> ParseResult<'a, &'a str> {
    let (rem, bytes) = take_n_padded(unpadded_len, buf)?;
    let value =
        std::str::from_utf8(bytes).map_err(|e| nom::Err::Failure(ParseError::InvalidUtf8(e)))?;
    Ok((rem, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RawTraceRecord;

    #[test]
    fn empty_string() {
        let (trailing, empty) = parse_padded_string(0, &[1, 1, 1, 1]).unwrap();
        assert_eq!(empty, "");
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn string_no_padding() {
        let mut buf = "helloooo".as_bytes().to_vec(); // contents
        buf.extend([1, 1, 1, 1]); // trailing

        let (trailing, parsed) = parse_padded_string(8, &buf).unwrap();
        assert_eq!(parsed, "helloooo");
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn string_with_padding() {
        let mut buf = "hello".as_bytes().to_vec();
        buf.extend(&[0, 0, 0]); // padding
        buf.extend([1, 1, 1, 1]); // trailing

        let (trailing, parsed) = parse_padded_string(5, &buf).unwrap();
        assert_eq!(parsed, "hello");
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn string_ref_index() {
        let (trailing, parsed) = StringRef::parse(10u16, &[1, 1, 1, 1]).unwrap();
        assert_eq!(parsed, StringRef::Index(NonZeroU16::new(10).unwrap()));
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn string_ref_inline() {
        let mut buf = "hello".as_bytes().to_vec();
        buf.extend(&[0, 0, 0]); // padding
        buf.extend([1, 1, 1, 1]); // trailing

        let (trailing, parsed) = StringRef::parse(5 | STRING_REF_INLINE_BIT, &buf).unwrap();
        assert_eq!(parsed, StringRef::Inline("hello"),);
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn string_record() {
        let mut header = StringHeader::empty();
        header.set_string_index(10);
        header.set_string_len(9);

        assert_parses_to_record!(
            crate::fxt_builder::FxtBuilder::new(header).atom("hellooooo").build(),
            RawTraceRecord::String(StringRecord { index: 10, value: "hellooooo" }),
        );
    }
}
