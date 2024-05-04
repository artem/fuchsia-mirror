// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ie::Ie;
use {
    crate::{header::HeaderDefinition, ie::IeDefinition},
    proc_macro::TokenStream,
    quote::quote,
    std::collections::HashMap,
    syn::{
        braced,
        parse::{Parse, ParseStream},
        parse_macro_input,
        punctuated::Punctuated,
        spanned::Spanned,
        Error, Expr, Ident, Result, Token,
    },
};

macro_rules! unwrap_or_bail {
    ($x:expr) => {
        match $x {
            Err(e) => return TokenStream::from(e.to_compile_error()),
            Ok(x) => x,
        }
    };
}

const GROUP_NAME_HEADERS: &str = "headers";
const GROUP_NAME_BODY: &str = "body";
const GROUP_NAME_IES: &str = "ies";
const GROUP_NAME_PAYLOAD: &str = "payload";

/// A set of `BufferWrite` types which can be written into a buffer.
enum Writeable {
    Header(HeaderDefinition),
    Body(Expr),
    Ie(IeDefinition),
    Payload(Expr),
}

pub trait BufferWrite {
    fn gen_frame_len_tokens(&self) -> Result<proc_macro2::TokenStream>;
    fn gen_write_to_buf_tokens(&self) -> Result<proc_macro2::TokenStream>;
    fn gen_var_declaration_tokens(&self) -> Result<proc_macro2::TokenStream>;
}

impl BufferWrite for Writeable {
    fn gen_frame_len_tokens(&self) -> Result<proc_macro2::TokenStream> {
        match self {
            Writeable::Header(x) => x.gen_frame_len_tokens(),
            Writeable::Body(_) => Ok(quote!(frame_len += body.len();)),
            Writeable::Ie(x) => x.gen_frame_len_tokens(),
            Writeable::Payload(_) => Ok(quote!(frame_len += payload.len();)),
        }
    }
    fn gen_write_to_buf_tokens(&self) -> Result<proc_macro2::TokenStream> {
        match self {
            Writeable::Header(x) => x.gen_write_to_buf_tokens(),
            Writeable::Body(_) => Ok(quote!(w.append_value(&body[..])?;)),
            Writeable::Ie(x) => x.gen_write_to_buf_tokens(),
            Writeable::Payload(_) => Ok(quote!(w.append_value(&payload[..])?;)),
        }
    }
    fn gen_var_declaration_tokens(&self) -> Result<proc_macro2::TokenStream> {
        match self {
            Writeable::Header(x) => x.gen_var_declaration_tokens(),
            Writeable::Body(x) => Ok(quote!(let body = #x;)),
            Writeable::Ie(x) => x.gen_var_declaration_tokens(),
            Writeable::Payload(x) => Ok(quote!(let payload = #x;)),
        }
    }
}
struct MacroArgs {
    buffer_source: Expr,
    write_defs: WriteDefinitions,
}

impl Parse for MacroArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let buffer_source = input.parse::<Expr>()?;
        input.parse::<Token![,]>()?;
        let write_defs = input.parse::<WriteDefinitions>()?;
        Ok(MacroArgs { buffer_source, write_defs })
    }
}

/// A parseable struct representing the macro's arguments.
struct WriteDefinitions {
    hdrs: Vec<Writeable>,
    body: Option<Writeable>,
    fields: Vec<Writeable>,
    payload: Option<Writeable>,
}

impl Parse for WriteDefinitions {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let content;
        braced!(content in input);
        let groups = Punctuated::<GroupArgs, Token![,]>::parse_terminated(&content)?;

        let mut hdrs = vec![];
        let mut fields = vec![];
        let mut body = None;
        let mut payload = None;
        for group in groups {
            match group {
                GroupArgs::Headers(data) => {
                    hdrs.extend(data.into_iter().map(|x| Writeable::Header(x)));
                }
                GroupArgs::Fields(data) => {
                    fields.extend(data.into_iter().map(|x| Writeable::Ie(x)));
                }
                GroupArgs::Payload(data) => {
                    if payload.is_some() {
                        return Err(Error::new(data.span(), "more than one payload defined"));
                    }
                    payload.replace(Writeable::Payload(data));
                }
                GroupArgs::Body(data) => {
                    if body.is_some() {
                        return Err(Error::new(data.span(), "more than one body defined"));
                    }
                    body.replace(Writeable::Body(data));
                }
            }
        }

        Ok(Self { hdrs, fields, body, payload })
    }
}

/// A parseable struct representing an individual group of definitions such as headers, IEs or
/// the buffer provider.
enum GroupArgs {
    Headers(Vec<HeaderDefinition>),
    Body(Expr),
    Fields(Vec<IeDefinition>),
    Payload(Expr),
}

impl Parse for GroupArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;

        match name.to_string().as_str() {
            GROUP_NAME_HEADERS => {
                let content;
                braced!(content in input);
                let hdrs: Punctuated<HeaderDefinition, Token![,]> =
                    Punctuated::parse_terminated(&content)?;
                Ok(GroupArgs::Headers(hdrs.into_iter().collect()))
            }
            GROUP_NAME_IES => {
                let content;
                braced!(content in input);
                let ies: Punctuated<IeDefinition, Token![,]> =
                    Punctuated::parse_terminated(&content)?;
                let ies = ies.into_iter().collect::<Vec<_>>();

                // Error if a IE was defined more than once.
                let mut map = HashMap::new();
                for ie in &ies {
                    if let Some(_) = map.insert(ie.type_, ie) {
                        return Err(Error::new(ie.name.span(), "IE defined twice"));
                    }
                }

                let rates_presence =
                    map.iter().fold((false, None), |acc, (type_, ie)| match type_ {
                        Ie::ExtendedRates { .. } => (acc.0, Some(ie)),
                        Ie::Rates => (true, None),
                        _ => acc,
                    });
                if let (false, Some(ie)) = rates_presence {
                    return Err(Error::new(
                        ie.name.span(),
                        "`extended_supported_rates` IE specified without `supported_rates` IE",
                    ));
                }

                Ok(GroupArgs::Fields(ies))
            }
            GROUP_NAME_BODY => Ok(GroupArgs::Body(input.parse::<Expr>()?)),
            GROUP_NAME_PAYLOAD => Ok(GroupArgs::Payload(input.parse::<Expr>()?)),
            unknown => Err(Error::new(name.span(), format!("unknown group: '{}'", unknown))),
        }
    }
}

fn process_write_definitions(
    write_defs: WriteDefinitions,
    make_buf_tokens: proc_macro2::TokenStream,
    return_buf_tokens: proc_macro2::TokenStream,
) -> TokenStream {
    let mut declare_var_tokens = quote!();
    let mut write_to_buf_tokens = quote!();
    let mut frame_len_tokens = quote!(let mut frame_len = 0;);

    // Order writable pieces,=: Hdr + Body + Fields + Payload
    let mut writables = vec![];
    writables.extend(write_defs.hdrs);
    if let Some(x) = write_defs.body {
        writables.push(x);
    }
    writables.extend(write_defs.fields);
    if let Some(x) = write_defs.payload {
        writables.push(x);
    }

    for x in writables {
        let tokens = unwrap_or_bail!(x.gen_write_to_buf_tokens());
        write_to_buf_tokens = quote!(#write_to_buf_tokens #tokens);

        let tokens = unwrap_or_bail!(x.gen_frame_len_tokens());
        frame_len_tokens = quote!(#frame_len_tokens #tokens);

        let tokens = unwrap_or_bail!(x.gen_var_declaration_tokens());
        declare_var_tokens = quote!(#declare_var_tokens #tokens);
    }

    TokenStream::from(quote! {
        || -> Result<(_, usize), Error> {
            use {
                wlan_common::{
                    appendable::Appendable,
                    buffer_writer::BufferWriter,
                    error::FrameWriteError,
                    ie::{self, IE_PREFIX_LEN, SUPPORTED_RATES_MAX_LEN},
                },
                std::convert::AsRef,
                std::mem::size_of,
            };

            #declare_var_tokens
            #frame_len_tokens

            #make_buf_tokens

            {
                #write_to_buf_tokens
            }

            #return_buf_tokens
        }()
    })
}

pub fn process_with_buffer_provider(input: TokenStream) -> TokenStream {
    let macro_args = parse_macro_input!(input as MacroArgs);
    let buffer_source = macro_args.buffer_source;
    let buf_tokens = quote!(
        let mut buffer_provider: &::wlan_frame_writer::__BufferProvider = &#buffer_source;
        let mut buffer = buffer_provider.get_buffer(frame_len)?;
        let mut w = BufferWriter::new(&mut buffer[..]);
    );
    let return_buf_tokens = quote!(
        let written = w.written();
        Ok((buffer, written))
    );
    process_write_definitions(macro_args.write_defs, buf_tokens, return_buf_tokens)
}

pub fn process_with_dynamic_buffer(input: TokenStream) -> TokenStream {
    let macro_args = parse_macro_input!(input as MacroArgs);
    let buffer_source = macro_args.buffer_source;
    let buf_tokens = quote!(
        let mut w = #buffer_source;
    );
    let return_buf_tokens = quote!(
        let written = w.bytes_written();
        Ok((w, written))
    );
    process_write_definitions(macro_args.write_defs, buf_tokens, return_buf_tokens)
}

pub fn process_with_fixed_buffer(input: TokenStream) -> TokenStream {
    let macro_args = parse_macro_input!(input as MacroArgs);
    let buffer_source = macro_args.buffer_source;
    let buf_tokens = quote!(
        let mut buffer = #buffer_source;
        let mut w = BufferWriter::new(&mut buffer[..]);
    );
    let return_buf_tokens = quote!(
        let written = w.bytes_written();
        Ok((buffer, written))
    );
    process_write_definitions(macro_args.write_defs, buf_tokens, return_buf_tokens)
}
