// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the AT command AST.
//! These tests use the AT commands defined in examples.at.

#![cfg(test)]

use {
    crate::{
        generated::translate, generated::types as highlevel, lowlevel, lowlevel::write_to::WriteTo,
        parser::command_parser, serde::SerDe,
    },
    std::{collections::HashMap, io::Cursor},
};

fn cr_terminate(str: &str) -> String {
    format!("{}\r", str)
}

// This function tests that the raise, lower, parse and write funtions all compose and round trip as
// expected, and that the serde methods which compose them continue to work as well.  It takes a
// highlevel representation, a lowlevel representation and a string representation of the same AT
// command and tests that converting between them in all possible ways produce the expected values.
fn test_roundtrips(highlevel: highlevel::Command, lowlevel: lowlevel::Command, string: String) {
    // TEST I: highlevel -> lowlevel -> bytes -> lowlevel -> highlevel
    let mut bytes_from_lowlevel = Vec::new();

    // Do round trip
    let lowlevel_from_highlevel = translate::lower_command(&highlevel);
    lowlevel_from_highlevel.write_to(&mut bytes_from_lowlevel).expect("Failed to write lowlevel.");
    // TODO(fxb/66041) Convert parse to use Read rather than strings.
    let string_from_lowlevel =
        String::from_utf8(bytes_from_lowlevel).expect("Failed to convert bytes to UFT8.");
    let lowlevel_from_bytes =
        command_parser::parse(&string_from_lowlevel).expect("Failed to parse bytes.");
    let highlevel_from_lowlevel =
        translate::raise_command(&lowlevel_from_bytes).expect("Failed to raise lowlevel.");

    // Assert all the things are equal.
    assert_eq!(highlevel, highlevel_from_lowlevel);
    assert_eq!(lowlevel, lowlevel_from_highlevel);
    assert_eq!(lowlevel, lowlevel_from_bytes);
    assert_eq!(string, string_from_lowlevel);

    // TEST II: highlevel -> bytes -> highlevel
    // This should be identical to above assuming SerDe::serialize and
    // SerDe::deserialize are implemented correctly.
    let mut bytes_from_highlevel = Vec::new();

    // Do round trip
    highlevel.serialize(&mut bytes_from_highlevel).expect("Failed to serialize highlevel.");
    let highlevel_from_bytes =
        highlevel::Command::deserialize(&mut Cursor::new(bytes_from_highlevel.clone()))
            .expect("Failed to deserialize bytes.");

    // Convert to a String so errors are human readable, not just hex.
    let string_from_highlevel =
        String::from_utf8(bytes_from_highlevel).expect("Failed to convert bytes to UFT8.");

    // Assert all the things are equal.
    assert_eq!(highlevel, highlevel_from_bytes);
    assert_eq!(string, string_from_highlevel);

    // TEST III: bytes -> lowlevel -> highlevel -> lowlevel -> bytes
    let mut bytes_from_lowlevel = Vec::new();

    // Do round trip
    // TODO(fxb/66041) Convert parse to use Read rather than strings.
    let lowlevel_from_bytes = command_parser::parse(&string).expect("Failed to parse String.");
    let highlevel_from_lowlevel =
        translate::raise_command(&lowlevel_from_bytes).expect("Failed to raise lowlevel.");
    let lowlevel_from_highlevel = translate::lower_command(&highlevel_from_lowlevel);
    lowlevel_from_highlevel.write_to(&mut bytes_from_lowlevel).expect("Failed to write lowlevel.");

    // Convert to a String so errors are human readable, not just hex.
    let string_from_lowlevel =
        String::from_utf8(bytes_from_lowlevel).expect("Failed to convert bytes to UTF8.");

    // Assert all the things are equal.
    assert_eq!(highlevel, highlevel_from_lowlevel);
    assert_eq!(lowlevel, lowlevel_from_highlevel);
    assert_eq!(lowlevel, lowlevel_from_bytes);
    assert_eq!(string, string_from_lowlevel);

    // TEST IV: bytes -> highlevel -> bytes
    // This should be identical to above assuming SerDe::serialize and
    // SerDe::deserialize are implemented correctly.
    let mut bytes_from_highlevel = Vec::new();

    // Do round trip
    let highlevel_from_bytes = highlevel::Command::deserialize(&mut Cursor::new(string.clone()))
        .expect("Failed to deserialize String.");
    highlevel_from_bytes.serialize(&mut bytes_from_highlevel).expect("Failed to serialize bytes.");

    // Convert to a String so errors are human readable, not just hex.
    let string_from_highlevel =
        String::from_utf8(bytes_from_highlevel).expect("Fialed to convert bytes to UTF8.");

    // Assert all the things are equal.
    assert_eq!(highlevel, highlevel_from_bytes);
    assert_eq!(string, string_from_highlevel);
}

// Execute command with no arguments
#[test]
fn exec_no_args() {
    test_roundtrips(
        highlevel::Command::Testex {},
        lowlevel::Command::Execute {
            name: String::from("TESTEX"),
            is_extension: false,
            arguments: None,
        },
        cr_terminate("ATTESTEX"),
    )
}

// Extension execute command with no arguments
#[test]
fn exec_ext_no_args() {
    test_roundtrips(
        highlevel::Command::Testexext {},
        lowlevel::Command::Execute {
            name: String::from("TESTEXEXT"),
            is_extension: true,
            arguments: None,
        },
        cr_terminate("AT+TESTEXEXT"),
    )
}

// Extension execute command with one integer argument
#[test]
fn exec_one_int_arg() {
    test_roundtrips(
        highlevel::Command::Testexextfi { field: 1 },
        lowlevel::Command::Execute {
            name: String::from("TESTEXEXTFI"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ArgumentList(vec![
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::Integer(1)),
                ]),
            }),
        },
        cr_terminate("AT+TESTEXEXTFI=1"),
    )
}

// Extension execute command with one string argument
#[test]
fn exec_one_string_arg() {
    test_roundtrips(
        highlevel::Command::Testexextfs { field: String::from("abc") },
        lowlevel::Command::Execute {
            name: String::from("TESTEXEXTFS"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ArgumentList(vec![
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::String(
                        String::from("abc"),
                    )),
                ]),
            }),
        },
        cr_terminate("AT+TESTEXEXTFS=abc"),
    )
}

// Extension execute command with one key-value argument for a map
#[test]
fn exec_one_kv_arg() {
    let mut map = HashMap::new();
    map.insert(1, String::from("abc"));

    test_roundtrips(
        highlevel::Command::Testm { field: map },
        lowlevel::Command::Execute {
            name: String::from("TESTM"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ArgumentList(vec![
                    lowlevel::Argument::KeyValueArgument {
                        key: lowlevel::PrimitiveArgument::Integer(1),
                        value: lowlevel::PrimitiveArgument::String(String::from("abc")),
                    },
                ]),
            }),
        },
        cr_terminate("AT+TESTM=1=abc"),
    )
}

// Extension execute command with multiple arguments for a list
#[test]
fn exec_list() {
    test_roundtrips(
        highlevel::Command::Testl { field: vec![1, 2] },
        lowlevel::Command::Execute {
            name: String::from("TESTL"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ArgumentList(vec![
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::Integer(1)),
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::Integer(2)),
                ]),
            }),
        },
        cr_terminate("AT+TESTL=1,2"),
    )
}

// Extension execute command with multiple arguments
#[test]
fn exec_args() {
    test_roundtrips(
        highlevel::Command::Testexextfsi { field1: String::from("abc"), field2: 1 },
        lowlevel::Command::Execute {
            name: String::from("TESTEXEXTFSI"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ArgumentList(vec![
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::String(
                        String::from("abc"),
                    )),
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::Integer(1)),
                ]),
            }),
        },
        cr_terminate("AT+TESTEXEXTFSI=abc,1"),
    )
}

// Paren delimited argument list
#[test]
fn paren_args() {
    test_roundtrips(
        highlevel::Command::Testp { field: 1 },
        lowlevel::Command::Execute {
            name: String::from("TESTP"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ParenthesisDelimitedArgumentLists(vec![vec![
                    lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::Integer(1)),
                ]]),
            }),
        },
        cr_terminate("AT+TESTP=(1)"),
    )
}

// Paren delimited multiple argument lists
#[test]
fn multiple_paren_args() {
    test_roundtrips(
        highlevel::Command::Testpp { field1: 1, field2: 2, field3: String::from("abc") },
        lowlevel::Command::Execute {
            name: String::from("TESTPP"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ParenthesisDelimitedArgumentLists(vec![
                    vec![lowlevel::Argument::PrimitiveArgument(
                        lowlevel::PrimitiveArgument::Integer(1),
                    )],
                    vec![
                        lowlevel::Argument::PrimitiveArgument(
                            lowlevel::PrimitiveArgument::Integer(2),
                        ),
                        lowlevel::Argument::PrimitiveArgument(lowlevel::PrimitiveArgument::String(
                            String::from("abc"),
                        )),
                    ],
                ]),
            }),
        },
        cr_terminate("AT+TESTPP=(1)(2,abc)"),
    )
}

// Paren delimited multiple argument lists with key-value elements and lists
#[test]
fn multiple_paren_kv_args() {
    let mut map = HashMap::new();
    map.insert(1, String::from("abc"));

    test_roundtrips(
        highlevel::Command::Testpmpil { field1: map, field2: 2, field3: vec![3, 4] },
        lowlevel::Command::Execute {
            name: String::from("TESTPMPIL"),
            is_extension: true,
            arguments: Some(lowlevel::ExecuteArguments {
                nonstandard_delimiter: None,
                arguments: lowlevel::Arguments::ParenthesisDelimitedArgumentLists(vec![
                    vec![lowlevel::Argument::KeyValueArgument {
                        key: lowlevel::PrimitiveArgument::Integer(1),
                        value: lowlevel::PrimitiveArgument::String(String::from("abc")),
                    }],
                    vec![
                        lowlevel::Argument::PrimitiveArgument(
                            lowlevel::PrimitiveArgument::Integer(2),
                        ),
                        lowlevel::Argument::PrimitiveArgument(
                            lowlevel::PrimitiveArgument::Integer(3),
                        ),
                        lowlevel::Argument::PrimitiveArgument(
                            lowlevel::PrimitiveArgument::Integer(4),
                        ),
                    ],
                ]),
            }),
        },
        cr_terminate("AT+TESTPMPIL=(1=abc)(2,3,4)"),
    )
}

// Read command
#[test]
fn read() {
    test_roundtrips(
        highlevel::Command::TestrRead {},
        lowlevel::Command::Read { name: String::from("TESTR"), is_extension: false },
        cr_terminate("ATTESTR?"),
    )
}

// Extension read command
#[test]
fn read_ext() {
    test_roundtrips(
        highlevel::Command::TestrexRead {},
        lowlevel::Command::Read { name: String::from("TESTREX"), is_extension: true },
        cr_terminate("AT+TESTREX?"),
    )
}

// Test command
#[test]
fn test() {
    test_roundtrips(
        highlevel::Command::TesttTest {},
        lowlevel::Command::Test { name: String::from("TESTT"), is_extension: false },
        cr_terminate("ATTESTT=?"),
    )
}

// Extension test command
#[test]
fn test_ext() {
    test_roundtrips(
        highlevel::Command::TesttexTest {},
        lowlevel::Command::Test { name: String::from("TESTTEX"), is_extension: true },
        cr_terminate("AT+TESTTEX=?"),
    )
}
