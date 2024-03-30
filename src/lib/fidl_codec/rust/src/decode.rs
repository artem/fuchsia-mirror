// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::encoding::{TransactionHeader, ALLOC_PRESENT_U32, ALLOC_PRESENT_U64};
use nom::bytes::complete::take;
use nom::combinator::{map, value, verify};
use nom::multi::count;
use nom::sequence::{pair, preceded, terminated, tuple};
use nom::IResult;

use crate::error::{Error, Result};
use crate::library;
use crate::util::*;
use crate::value::Value;

use std::str;

type DResult<'a, R> = IResult<&'a [u8], R, Error>;

/// This represents an action that will yield a Value when given further bytes to process and
/// handles to potentially consume. This is how we implement out-of-line data. The initial parse
/// takes the inline data, and when we're ready, the Defer can be fed the remaining bytes to take
/// the out of line data.
enum Defer<'d> {
    /// This Defer doesn't need any further processing. We can just offer up the value right now.
    Complete(Value),

    /// This Defer implements the actual deferred processing pattern described.
    Action(
        Box<
            dyn for<'a> FnOnce(
                    &'a [u8],
                    &mut Vec<fidl::HandleInfo>,
                    RecursionCounter,
                ) -> DResult<'a, Value>
                + 'd,
        >,
    ),
}

impl<'d> Defer<'d> {
    /// Completes a deferred parse and returns the result.
    fn complete<'a>(
        self,
        data: &'a [u8],
        handles: &mut Vec<fidl::HandleInfo>,
        counter: RecursionCounter,
    ) -> DResult<'a, Value> {
        match self {
            Defer::Complete(v) => Ok((data, v)),
            Defer::Action(act) => act(data, handles, counter),
        }
    }
}

impl<'d> From<Value> for Defer<'d> {
    fn from(v: Value) -> Defer<'d> {
        Defer::Complete(v)
    }
}

fn take_u8(data: &[u8]) -> DResult<'_, u8> {
    map(take(1usize), |x: &[u8]| x[0])(data)
}

fn value_u8(data: &[u8]) -> DResult<'_, Value> {
    map(take_u8, Value::U8)(data)
}

fn value_bool(data: &[u8]) -> DResult<'_, Value> {
    map(verify(take_u8, |&x| x == 0 || x == 1), |x| Value::Bool(x != 0))(data)
}

fn take_u16(data: &[u8]) -> DResult<'_, u16> {
    map(take(2usize), |x: &[u8]| u16::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_u16(data: &[u8]) -> DResult<'_, Value> {
    map(take_u16, Value::U16)(data)
}

fn take_u32(data: &[u8]) -> DResult<'_, u32> {
    map(take(4usize), |x: &[u8]| u32::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_u32(data: &[u8]) -> DResult<'_, Value> {
    map(take_u32, Value::U32)(data)
}

fn take_u64(data: &[u8]) -> DResult<'_, u64> {
    map(take(8usize), |x: &[u8]| u64::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_u64(data: &[u8]) -> DResult<'_, Value> {
    map(take_u64, Value::U64)(data)
}

fn take_i8(data: &[u8]) -> DResult<'_, i8> {
    map(take(1usize), |x: &[u8]| i8::from_le_bytes([x[0]]))(data)
}

fn value_i8(data: &[u8]) -> DResult<'_, Value> {
    map(take_i8, Value::I8)(data)
}

fn take_i16(data: &[u8]) -> DResult<'_, i16> {
    map(take(2usize), |x: &[u8]| i16::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_i16(data: &[u8]) -> DResult<'_, Value> {
    map(take_i16, Value::I16)(data)
}

fn take_i32(data: &[u8]) -> DResult<'_, i32> {
    map(take(4usize), |x: &[u8]| i32::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_i32(data: &[u8]) -> DResult<'_, Value> {
    map(take_i32, Value::I32)(data)
}

fn take_i64(data: &[u8]) -> DResult<'_, i64> {
    map(take(8usize), |x: &[u8]| i64::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_i64(data: &[u8]) -> DResult<'_, Value> {
    map(take_i64, Value::I64)(data)
}

fn take_f32(data: &[u8]) -> DResult<'_, f32> {
    map(take(4usize), |x: &[u8]| f32::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_f32(data: &[u8]) -> DResult<'_, Value> {
    map(take_f32, Value::F32)(data)
}

fn take_f64(data: &[u8]) -> DResult<'_, f64> {
    map(take(8usize), |x: &[u8]| f64::from_le_bytes(x.try_into().unwrap()))(data)
}

fn value_f64(data: &[u8]) -> DResult<'_, Value> {
    map(take_f64, Value::F64)(data)
}

fn transaction_header(data: &[u8]) -> DResult<'_, TransactionHeader> {
    fidl::encoding::decode_transaction_header(data)
        .map(|(a, b)| (b, a))
        .map_err(|e| Error::DecodeError(format!("Invalid FIDL transaction header ({e:?})")).into())
}

fn take_padding(amount: usize) -> impl Fn(&[u8]) -> DResult<'_, ()> {
    move |bytes| value((), verify(take(amount), |x: &[u8]| x.iter().all(|&x| x == 0)))(bytes)
}

fn decode_struct<'s>(
    ns: &'s library::Namespace,
    st: &'s library::Struct,
    nullable: bool,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'s>> {
    move |bytes: &[u8]| {
        if !nullable {
            return decode_struct_nonnull(ns, st)(bytes);
        }

        let (bytes, presence) = take_u64(bytes)?;

        if presence == 0 {
            Ok((bytes, Defer::Complete(Value::Null)))
        } else if presence != ALLOC_PRESENT_U64 {
            Err(Error::DecodeError("Bad presence indicator".to_owned()).into())
        } else {
            Ok((
                bytes,
                Defer::Action(Box::new(
                    move |bytes: &[u8],
                          handles: &mut Vec<fidl::HandleInfo>,
                          counter: RecursionCounter| {
                        let counter = counter.next()?;
                        let align = alignment_padding_for_size(st.size);
                        let (bytes, defer) =
                            terminated(decode_struct_nonnull(ns, st), take_padding(align))(bytes)?;
                        defer.complete(bytes, handles, counter)
                    },
                )),
            ))
        }
    }
}

fn decode_struct_nonnull<'s>(
    ns: &'s library::Namespace,
    st: &'s library::Struct,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'s>> {
    move |mut bytes: &[u8]| {
        let mut offset = 0;
        let mut fields = Vec::new();

        for member in &st.members {
            let (remaining, result) =
                preceded(take_padding(member.offset - offset), decode_type(ns, &member.ty))(bytes)?;
            fields.push((member.name.clone(), result));
            bytes = remaining;
            offset = member.offset + member.ty.inline_size(ns)?;
        }

        if offset < st.size {
            let (remaining, _) = take_padding(st.size - offset)(bytes)?;
            bytes = remaining;
        }

        Ok((
            bytes,
            Defer::Action(Box::new(
                move |mut bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let mut complete_fields = Vec::new();

                    for (name, defer) in fields {
                        let (remaining, value) = defer.complete(bytes, handles, counter)?;
                        bytes = remaining;
                        complete_fields.push((name, value))
                    }

                    Ok((bytes, Value::Object(complete_fields)))
                },
            )),
        ))
    }
}

fn decode_type<'t>(
    ns: &'t library::Namespace,
    ty: &'t library::Type,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'t>> {
    use library::Type;
    move |b: &[u8]| {
        match ty {
            Type::Bool => value_bool(b),
            Type::U8 => value_u8(b),
            Type::U16 => value_u16(b),
            Type::U32 => value_u32(b),
            Type::U64 => value_u64(b),
            Type::I8 => value_i8(b),
            Type::I16 => value_i16(b),
            Type::I32 => value_i32(b),
            Type::I64 => value_i64(b),
            Type::F32 => value_f32(b),
            Type::F64 => value_f64(b),
            Type::Array(ty, size) => return decode_array(ns, ty, *size)(b),
            Type::Vector { ty, nullable, element_count } => {
                return decode_vector(ns, ty, *nullable, *element_count)(b)
            }
            Type::String { nullable, byte_count } => {
                return decode_string(*nullable, *byte_count)(b)
            }
            Type::Identifier { name, nullable } => {
                return decode_identifier(ns, name, *nullable)(b)
            }
            Type::Handle { object_type, nullable, rights } => {
                return decode_handle(*object_type, *nullable, *rights)(b)
            }
            Type::Request { identifier, rights, nullable } => {
                return decode_server_end(identifier.clone(), *nullable, *rights)(b)
            }
            Type::UnknownString(s) => {
                Err(Error::LibraryError(format!("Unresolved Type: {}", s)).into())
            }
            Type::Unknown(library::TypeInfo { identifier: s, .. }) => {
                return Err(Error::LibraryError(format!(
                    "Unresolved Type: {}",
                    s.as_ref().unwrap_or(&"<unidentified>".to_owned())
                ))
                .into())
            }
            Type::FrameworkError => map(take_u32, |_| Value::Null)(b),
        }
        .map(|(x, y)| (x, Defer::Complete(y)))
    }
}

/// Given a list of defers, complete them all and turn them into a list of complete values.
fn complete_deferred_list<'a>(
    bytes: &'a [u8],
    handles: &mut Vec<fidl::HandleInfo>,
    defers: Vec<Defer<'_>>,
    counter: RecursionCounter,
) -> DResult<'a, Value> {
    let mut bytes = bytes;
    let mut values = Vec::new();

    for defer in defers {
        let (next_bytes, value) = defer.complete(bytes, handles, counter)?;
        bytes = next_bytes;
        values.push(value)
    }

    Ok((bytes, Value::List(values)))
}

fn decode_array<'t>(
    ns: &'t library::Namespace,
    ty: &'t library::Type,
    size: usize,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'t>> {
    move |bytes: &[u8]| {
        let (bytes, defers) = count(decode_type(ns, ty), size)(bytes)?;

        Ok((
            bytes,
            Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    complete_deferred_list(bytes, handles, defers, counter)
                },
            )),
        ))
    }
}

fn decode_vector<'t>(
    ns: &'t library::Namespace,
    ty: &'t library::Type,
    nullable: bool,
    element_count: Option<usize>,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'t>> {
    move |bytes: &[u8]| {
        let (bytes, (size, presence)) = pair(take_u64, take_u64)(bytes)?;
        let size = size as usize;
        let align = alignment_padding_for_size(size * ty.inline_size(ns)?);

        if presence == 0 {
            if nullable {
                if size == 0 {
                    Ok((bytes, Defer::Complete(Value::Null)))
                } else {
                    Err(Error::DecodeError("Absent vector had a size".to_owned()).into())
                }
            } else {
                Err(Error::DecodeError("Missing non-nullable vector".to_owned()).into())
            }
        } else if presence != ALLOC_PRESENT_U64 {
            Err(Error::DecodeError("Bad presence indicator".to_owned()).into())
        } else if element_count.map(|x| x < size).unwrap_or(false) {
            Err(Error::DecodeError("Vector too long".to_owned()).into())
        } else {
            Ok((
                bytes,
                Defer::Action(Box::new(
                    move |bytes: &[u8],
                          handles: &mut Vec<fidl::HandleInfo>,
                          counter: RecursionCounter| {
                        let counter = counter.next()?;
                        let (bytes, defers) = terminated(
                            count(decode_type(ns, ty), size),
                            take_padding(align),
                        )(bytes)?;

                        complete_deferred_list(bytes, handles, defers, counter)
                    },
                )),
            ))
        }
    }
}

fn decode_string(
    nullable: bool,
    byte_count: Option<usize>,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'static>> {
    move |bytes: &[u8]| {
        let (bytes, (size, presence)) = pair(take_u64, take_u64)(bytes)?;
        let size = size as usize;
        let align = alignment_padding_for_size(size);

        if presence == 0 {
            if nullable {
                if size == 0 {
                    Ok((bytes, Defer::Complete(Value::Null)))
                } else {
                    Err(Error::DecodeError("Absent string had a size".to_owned()).into())
                }
            } else {
                Err(Error::DecodeError("Missing non-nullable string".to_owned()).into())
            }
        } else if presence != ALLOC_PRESENT_U64 {
            Err(Error::DecodeError("Bad presence indicator".to_owned()).into())
        } else if byte_count.map(|x| x < size).unwrap_or(false) {
            Err(Error::DecodeError("String too long".to_owned()).into())
        } else {
            Ok((
                bytes,
                Defer::Action(Box::new(
                    move |bytes: &[u8],
                          _: &mut Vec<fidl::HandleInfo>,
                          counter: RecursionCounter| {
                        let _counter = counter.next()?;
                        let (bytes, data) = terminated(take(size), take_padding(align))(bytes)?;

                        match str::from_utf8(data) {
                            Ok(x) => Ok((bytes, Value::String(x.to_owned()))),
                            Err(x) => Err(Error::Utf8Error(x).into()),
                        }
                    },
                )),
            ))
        }
    }
}

fn decode_server_end(
    interface: String,
    nullable: bool,
    rights: fidl::Rights,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'static>> {
    decode_handle_with(
        interface,
        nullable,
        &|x, y| Value::ServerEnd(x.into(), y),
        Some(fidl::ObjectType::CHANNEL),
        rights,
    )
}

fn decode_client_end(
    interface: String,
    nullable: bool,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'static>> {
    decode_handle_with(
        interface,
        nullable,
        &|x, y| Value::ClientEnd(x.into(), y),
        Some(fidl::ObjectType::CHANNEL),
        fidl::Rights::CHANNEL_DEFAULT,
    )
}

fn decode_handle(
    handle_type: fidl::ObjectType,
    nullable: bool,
    rights: fidl::Rights,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'static>> {
    decode_handle_with(handle_type, nullable, &Value::Handle, None, rights)
}

fn decode_handle_with<T: Clone + 'static>(
    handle_type: T,
    nullable: bool,
    value_builder: &'static (impl Fn(fidl::Handle, T) -> Value + 'static),
    constrain_type: Option<fidl::ObjectType>,
    rights: fidl::Rights,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'static>> {
    move |bytes: &[u8]| {
        let handle_type = handle_type.clone();
        let (bytes, presence) = take_u32(bytes)?;

        if presence == 0 {
            if nullable {
                Ok((bytes, Defer::Complete(Value::Null)))
            } else {
                Err(Error::DecodeError("Missing non-nullable handle".to_owned()).into())
            }
        } else if presence != ALLOC_PRESENT_U32 {
            Err(Error::DecodeError("Bad presence indicator".to_owned()).into())
        } else {
            Ok((
                bytes,
                Defer::Action(Box::new(
                    move |bytes: &[u8],
                          handles: &mut Vec<fidl::HandleInfo>,
                          _: RecursionCounter| {
                        if !handles.is_empty() {
                            if constrain_type.map(|x| x == handles[0].object_type).unwrap_or(true) {
                                let handle = handles.remove(0);
                                if !handle.rights.contains(rights)
                                    && handle.rights != fidl::Rights::SAME_RIGHTS
                                {
                                    let missing = rights.clone().remove(handle.rights);
                                    Err(Error::DecodeError(format!(
                                        "Insufficient handle rights, need {missing:?}"
                                    ))
                                    .into())
                                } else {
                                    Ok((bytes, value_builder(handle.handle, handle_type)))
                                }
                            } else {
                                Err(Error::DecodeError("Wrong handle type".to_owned()).into())
                            }
                        } else {
                            Err(Error::DecodeError("Too few handles".to_owned()).into())
                        }
                    },
                )),
            ))
        }
    }
}

fn decode_enum<'e>(
    ns: &'e library::Namespace,
    en: &'e library::Enum,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'e>> {
    move |bytes: &[u8]| {
        let (bytes, defer) = decode_type(ns, &en.ty)(bytes)?;
        Ok((
            bytes,
            Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let (bytes, value) = defer.complete(bytes, handles, counter)?;

                    for member in &en.members {
                        if value == member.value || !en.strict {
                            return Ok((bytes, Value::Enum(en.name.to_owned(), Box::new(value))));
                        }
                    }

                    if en.strict {
                        Err(Error::DecodeError("Unknown Enum Variant.".to_owned()).into())
                    } else {
                        Ok((bytes, Value::Enum(en.name.to_owned(), Box::new(value))))
                    }
                },
            )),
        ))
    }
}

fn decode_bits<'b>(
    ns: &'b library::Namespace,
    bits: &'b library::Bits,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'b>> {
    move |bytes: &[u8]| {
        let (bytes, defer) = decode_type(ns, &bits.ty)(bytes)?;
        Ok((
            bytes,
            Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let (bytes, value) = defer.complete(bytes, handles, counter)?;

                    let data = value
                        .bits()
                        .ok_or(Error::LibraryError("Bits with non-integer type.".to_owned()))?;

                    if bits.strict && data != data & bits.mask {
                        Err(Error::DecodeError("Invalid value for bits field.".to_owned()).into())
                    } else {
                        Ok((bytes, Value::Bits(bits.name.to_owned(), Box::new(value))))
                    }
                },
            )),
        ))
    }
}

/// Contents of an envelope header.
enum Envelope {
    Present { bytes: u32, handles: u16 },
    Inline { bytes: [u8; 4], handles: u16 },
    Empty,
}

impl Envelope {
    fn skip(&self) -> Defer<'static> {
        let (envelope_bytes, envelope_handles) = match self {
            Envelope::Present { bytes, handles } => (*bytes, *handles),
            Envelope::Inline { handles, .. } => (0, *handles),
            Envelope::Empty => return Defer::Complete(Value::Null),
        };

        Defer::Action(Box::new(
            move |bytes: &[u8], handles: &mut Vec<fidl::HandleInfo>, counter: RecursionCounter| {
                let _counter = counter.next()?;
                if (envelope_bytes & 7u32) != 0 {
                    return Err(Error::DecodeError("Invalid envelope size".to_owned()).into());
                }
                let envelope_bytes = envelope_bytes as usize;
                let envelope_handles = envelope_handles as usize;

                if envelope_handles > handles.len() {
                    Err(Error::DecodeError("Insufficient handles for envelope".to_owned()).into())
                } else if envelope_bytes > bytes.len() {
                    Err(Error::DecodeError("Insufficient bytes for envelope".to_owned()).into())
                } else {
                    *handles = handles.split_off(envelope_handles);
                    Ok((&bytes[envelope_bytes as usize..], Value::Null))
                }
            },
        ))
    }

    fn decode_type<'s>(
        &self,
        ns: &'s library::Namespace,
        ty: &'s library::Type,
    ) -> Result<Defer<'s>> {
        if let &Envelope::Empty = self {
            Ok(self.skip())
        } else if !ty.is_resolved(ns) {
            Ok(self.skip())
        } else if let Envelope::Inline { bytes, handles } = self {
            let (padding, ret) = decode_type(ns, ty)(bytes)?;
            take_padding(padding.len())(padding)?;
            let expect_handles = *handles as usize;
            Ok(Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let handle_count = handles.len();
                    let v = ret.complete(bytes, handles, counter)?;

                    let handles_used = handle_count - handles.len();
                    if handles_used != expect_handles {
                        Err(Error::DecodeError("Wrong number of handles in envelope".to_owned())
                            .into())
                    } else {
                        Ok(v)
                    }
                },
            )))
        } else if ty.inline_size(ns)? <= 4 {
            Err(Error::DecodeError("Envelope should be inline".to_owned()))
        } else {
            let Envelope::Present { bytes, handles } = self else { unreachable!() };
            let expect_bytes = *bytes as usize;
            let expect_handles = *handles as usize;
            Ok(Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let counter = counter.next()?;
                    let bytes_start = bytes.len();
                    let align = alignment_padding_for_size(ty.inline_size(ns)?);
                    let (bytes, defer) =
                        terminated(decode_type(ns, ty), take_padding(align))(bytes)?;

                    let handle_count = handles.len();
                    let (bytes, value) = defer.complete(bytes, handles, counter)?;
                    let handles_used = handle_count - handles.len();
                    let bytes_used = bytes_start - bytes.len();
                    if handles_used != expect_handles {
                        Err(Error::DecodeError("Wrong number of handles in envelope".to_owned())
                            .into())
                    } else if bytes_used != expect_bytes {
                        Err(Error::DecodeError("Wrong number of bytes in envelope".to_owned())
                            .into())
                    } else {
                        Ok((bytes, value))
                    }
                },
            )))
        }
    }

    fn take<'a>(empty_ok: bool) -> impl Fn(&'a [u8]) -> DResult<'a, Envelope> {
        move |bytes: &[u8]| {
            let (bytes, (envelope_bytes, envelope_handles, envelope_flags)) =
                tuple((take_u32, take_u16, take_u16))(bytes)?;

            if envelope_bytes == 0 && envelope_handles == 0 && envelope_flags == 0 {
                if !empty_ok {
                    Err(Error::DecodeError("Unexpected empty envelope.".to_owned()).into())
                } else {
                    Ok((bytes, Envelope::Empty))
                }
            } else if envelope_flags == 0 {
                Ok((bytes, Envelope::Present { bytes: envelope_bytes, handles: envelope_handles }))
            } else if envelope_flags == 1 {
                Ok((
                    bytes,
                    Envelope::Inline {
                        bytes: envelope_bytes.to_le_bytes(),
                        handles: envelope_handles,
                    },
                ))
            } else {
                Err(Error::DecodeError("Unknown evelope flags.".to_owned()).into())
            }
        }
    }
}

fn decode_union<'u>(
    ns: &'u library::Namespace,
    union: &'u library::TableOrUnion,
    nullable: bool,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'u>> {
    move |bytes: &[u8]| {
        let (bytes, (ordinal, envelope)) = tuple((take_u64, Envelope::take(nullable)))(bytes)?;

        match (ordinal, &envelope) {
            (0, Envelope::Empty) => return Ok((bytes, envelope.skip())),
            (0, _) => return Err(Error::DecodeError("Invalid Union block.".to_owned()).into()),
            _ => (),
        };

        match union.members.get(&ordinal) {
            None if union.strict => {
                Err(Error::DecodeError("Invalid Union ordinal.".to_owned()).into())
            }
            None => Ok((bytes, envelope.skip())),
            Some(member) => Ok((
                bytes,
                Defer::Action(Box::new(
                    move |bytes: &[u8],
                          handles: &mut Vec<fidl::HandleInfo>,
                          counter: RecursionCounter| {
                        let (bytes, inner) = envelope
                            .decode_type(ns, &member.ty)?
                            .complete(bytes, handles, counter)?;
                        Ok((
                            bytes,
                            Value::Union(
                                union.name.to_owned(),
                                member.name.to_owned(),
                                Box::new(inner),
                            ),
                        ))
                    },
                )),
            )),
        }
    }
}

fn decode_table<'t>(
    ns: &'t library::Namespace,
    table: &'t library::TableOrUnion,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'t>> {
    move |bytes: &[u8]| {
        let (bytes, (size, data_ptr)) = pair(take_u64, take_u64)(bytes)?;

        if data_ptr != ALLOC_PRESENT_U64 {
            return Err(Error::DecodeError("Bad presence indicator.".to_owned()).into());
        }

        Ok((
            bytes,
            Defer::Action(Box::new(
                move |bytes: &[u8],
                      handles: &mut Vec<fidl::HandleInfo>,
                      counter: RecursionCounter| {
                    let counter = counter.next()?;
                    let (mut bytes, envelopes) = count(Envelope::take(true), size as usize)(bytes)?;

                    let mut result = Vec::new();
                    let mut expect_ord = 1u64;
                    for envelope in envelopes {
                        let member = table.members.get(&expect_ord);
                        expect_ord += 1;

                        let next_bytes = if let Some(member) = member {
                            let (next_bytes, val) = envelope
                                .decode_type(ns, &member.ty)?
                                .complete(bytes, handles, counter)?;
                            if !matches!(val, Value::Null) {
                                result.push((member.name.clone(), val));
                            }
                            next_bytes
                        } else {
                            envelope.skip().complete(bytes, handles, counter)?.0
                        };

                        bytes = next_bytes;
                    }

                    Ok((bytes, Value::Object(result)))
                },
            )),
        ))
    }
}

fn decode_identifier<'s>(
    ns: &'s library::Namespace,
    name: &'s str,
    nullable: bool,
) -> impl Fn(&[u8]) -> DResult<'_, Defer<'s>> {
    move |bytes: &[u8]| match ns.lookup(name)? {
        library::LookupResult::Bits(b) => decode_bits(ns, b)(bytes),
        library::LookupResult::Enum(e) => decode_enum(ns, e)(bytes),
        library::LookupResult::Struct(s) => decode_struct(ns, s, nullable)(bytes),
        library::LookupResult::Union(u) => decode_union(ns, u, nullable)(bytes),
        library::LookupResult::Table(t) => decode_table(ns, t)(bytes),
        library::LookupResult::Protocol(i) => decode_client_end(i.name.clone(), nullable)(bytes),
    }
}

/// Decode a FIDL request or response, depending on the direction header.
fn decode_message<'a>(
    ns: &library::Namespace,
    direction: Direction,
    bytes: &'a [u8],
    mut handles: Vec<fidl::HandleInfo>,
) -> Result<(TransactionHeader, Value)> {
    let (bytes, header) = transaction_header(bytes)?;

    let (_, method) = ns.lookup_method_ordinal(header.ordinal)?;

    let (message, has) = match direction {
        Direction::Request => (method.request.as_ref(), method.has_request),
        Direction::Response => (method.response.as_ref(), method.has_response),
    };

    if let Some(message) = message {
        let (bytes, defer) = decode_type(ns, message)(bytes)?;
        let (bytes, value) = defer.complete(bytes, &mut handles, RecursionCounter::new())?;

        if !bytes.is_empty() && (bytes.len() >= 8 || bytes.iter().any(|x| *x != 0)) {
            Err(Error::DecodeError(format!("{} bytes left over.", bytes.len())))
        } else if !handles.is_empty() {
            Err(Error::DecodeError(format!("{} handles left over.", handles.len())))
        } else {
            Ok((header, value))
        }
    } else if !has {
        Err(Error::DecodeError(format!(
            "Header indicates method {}, which has no {}.",
            method.name,
            direction.to_string()
        )))
    } else {
        Ok((header, Value::Null))
    }
}

/// Decode a FIDL request from a byte buffer and a list of handles.
pub fn decode_request(
    ns: &library::Namespace,
    bytes: &[u8],
    handles: Vec<fidl::HandleInfo>,
) -> Result<(TransactionHeader, Value)> {
    decode_message(ns, Direction::Request, bytes, handles)
}

/// Decode a FIDL response from a byte buffer and a list of handles.
pub fn decode_response(
    ns: &library::Namespace,
    bytes: &[u8],
    handles: Vec<fidl::HandleInfo>,
) -> Result<(TransactionHeader, Value)> {
    decode_message(ns, Direction::Response, bytes, handles)
}

/// Decode a FIDL value.
pub fn decode<'a>(
    ns: &library::Namespace,
    ty: &str,
    bytes: &'a [u8],
    mut handles: Vec<fidl::HandleInfo>,
) -> Result<Value> {
    if bytes.len() % 8 != 0 {
        return Err(Error::DecodeError("Unaligned encoded object".to_owned()));
    }
    let (bytes, defer) = decode_identifier(ns, ty, false)(bytes)?;
    let (bytes, value) = defer.complete(bytes, &mut handles, RecursionCounter::new())?;
    take_padding(bytes.len())(bytes)?;

    if !bytes.is_empty() && (bytes.len() >= 8 || bytes.iter().any(|x| *x != 0)) {
        Err(Error::DecodeError(format!("{} bytes left over.", bytes.len())))
    } else if !handles.is_empty() {
        Err(Error::DecodeError(format!("{} handles left over.", handles.len())))
    } else {
        Ok(value)
    }
}
