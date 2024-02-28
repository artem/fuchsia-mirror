// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use assert_matches::assert_matches;
use fidl_table_validation::*;
use fidl_test_tablevalidation::{Example, VecOfExample, WrapExample};
use test_case::test_case;

#[test]
fn rejects_missing_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        #[fidl_field_type(required)]
        num: u32,
    }

    assert_matches!(
        Valid::try_from(Example { num: Some(10), ..Default::default() }),
        Ok(Valid { num: 10 })
    );

    assert_matches!(
        Valid::try_from(Example { num: None, ..Default::default() }),
        Err(ExampleValidationError::MissingField(ExampleMissingFieldError::Num))
    );
}

#[test]
fn sets_default_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        #[fidl_field_type(default = 22)]
        num: u32,
    }

    assert_matches!(Valid::try_from(Example::default()), Ok(Valid { num: 22 }));
}

#[test_case(u32::MAX, false)]
#[test_case(22, true)]
fn converts_default_fields(num: u32, expect_ok: bool) {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        #[fidl_field_type(default = 22)]
        // Note that u16 is smaller than the FIDL table type of u32 and
        // exercises the conversion.
        num: u16,
    }

    let got = Valid::try_from(Example { num: Some(num), ..Default::default() });
    if expect_ok {
        assert_matches!(got, Ok(Valid { num: got_num }) => assert_eq!(u32::from(got_num), num));
    } else {
        assert_matches!(got, Err(ExampleValidationError::InvalidField(_)))
    }
}

#[test]
fn accepts_optional_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        #[fidl_field_type(optional)]
        num: Option<u32>,
    }

    assert_matches!(Valid::try_from(Example::default()), Ok(Valid { num: None }));

    assert_matches!(
        Valid::try_from(Example { num: Some(15), ..Default::default() }),
        Ok(Valid { num: Some(15) })
    );
}

#[test]
fn runs_custom_validator() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    #[fidl_table_validator(ExampleValidator)]
    pub struct Valid {
        #[fidl_field_type(default = 10)]
        num: u32,
    }

    pub struct ExampleValidator;
    impl Validate<Valid> for ExampleValidator {
        type Error = ();
        fn validate(candidate: &Valid) -> Result<(), Self::Error> {
            match candidate.num {
                12 => Err(()),
                _ => Ok(()),
            }
        }
    }

    assert_matches!(
        Valid::try_from(Example { num: Some(10), ..Default::default() }),
        Ok(Valid { num: 10 })
    );

    assert_matches!(
        Valid::try_from(Example { num: Some(12), ..Default::default() }),
        Err(ExampleValidationError::Logical(()))
    );
}

#[test]
fn validates_nested_tables() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(WrapExample)]
    struct WrapValid {
        inner: Valid,
    }

    assert_matches!(
        WrapValid::try_from(WrapExample {
            inner: Some(Example { num: Some(10), ..Default::default() }),
            ..Default::default()
        }),
        Ok(WrapValid { inner: Valid { num: 10 } })
    );

    assert_matches!(
        WrapValid::try_from(WrapExample { inner: Some(Example::default()), ..Default::default() }),
        Err(WrapExampleValidationError::InvalidField(_))
    );

    // Can convert back to the nested FIDL table.
    assert_eq!(
        WrapExample::from(WrapValid { inner: Valid { num: 10 } }),
        WrapExample {
            inner: Some(Example { num: Some(10), ..Default::default() }),
            ..Default::default()
        }
    );
}

#[test]
fn works_with_qualified_type_names() {
    #[derive(Default, ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(fidl_test_tablevalidation::Example)]
    struct Valid {
        num: u32,
    }

    assert_matches!(
        Valid::try_from(Example { num: Some(7), ..Default::default() }),
        Ok(Valid { num: 7 })
    );
}

#[test]
fn works_with_option_wrapped_nested_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    #[derive(Default, ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(WrapExample)]
    struct WrapValid {
        #[fidl_field_type(optional)]
        inner: Option<Valid>,
    }

    assert_matches!(
        WrapValid::try_from(WrapExample {
            inner: Some(Example { num: Some(5), ..Default::default() }),
            ..Default::default()
        }),
        Ok(WrapValid { inner: Some(Valid { num: 5 }) })
    );
}

#[test]
fn works_with_vec_wrapped_nested_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    #[derive(Default, ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(VecOfExample)]
    struct VecOfValid {
        vec: Vec<Valid>,
    }

    assert_matches!(
        VecOfValid::try_from(VecOfExample {
            vec: Some(vec![
                Example { num: Some(5), ..Default::default() },
                Example { num: Some(6), ..Default::default() }
            ]),
            ..Default::default()
        }),
        Ok(VecOfValid { vec }) if vec == [Valid { num: 5 }, Valid { num: 6 }]
    );
}

#[test]
fn works_with_optional_vec_wrapped_nested_fields() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    #[derive(Default, ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(VecOfExample)]
    struct VecOfValid {
        #[fidl_field_type(optional)]
        vec: Option<Vec<Valid>>,
    }

    assert_matches!(
        VecOfValid::try_from(VecOfExample {
            vec: Some(vec![
                Example { num: Some(5), ..Default::default() },
                Example { num: Some(6), ..Default::default() }
            ]),
            ..Default::default()
        }),
        Ok(VecOfValid { vec: Some(vec) }) if vec == [Valid { num: 5 }, Valid { num: 6 }]
    );
}

#[test]
fn works_with_identifier_defaults() {
    const DEFAULT: u32 = 22;

    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        #[fidl_field_with_default(DEFAULT)]
        num: u32,
    }

    assert_matches!(Valid::try_from(Example::default()), Ok(Valid { num: DEFAULT }));
}

#[test]
fn works_with_default_impls() {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(VecOfExample)]
    struct VecOfValid {
        #[fidl_field_type(default)]
        vec: Vec<Example>,
    }

    assert_matches!(
        VecOfValid::try_from(VecOfExample::default()),
        Ok(VecOfValid { vec }) if vec.is_empty()
    );

    assert_matches!(
        VecOfValid::try_from(VecOfExample { vec: Some(vec![Example::default()]), ..Default::default() }),
        Ok(VecOfValid { vec }) if vec == [Example::default()]
    );
}

#[test_case(None, true)]
#[test_case(Some(Example::default()), false)]
fn works_with_nested_default(inner: Option<Example>, expect_ok: bool) {
    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    impl Default for Valid {
        fn default() -> Self {
            Self { num: 42 }
        }
    }

    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(WrapExample)]
    struct WrapValid {
        #[fidl_field_type(default)]
        inner: Valid,
    }

    let got = WrapValid::try_from(WrapExample { inner, ..Default::default() });
    if expect_ok {
        assert_matches!(
            got,
            Ok(WrapValid { inner }) => assert_eq!(inner, Default::default())
        );
    } else {
        assert_matches!(got, Err(WrapExampleValidationError::InvalidField(_)));
    }
}

#[test_case(None, true)]
#[test_case(Some(Example::default()), false)]
fn works_with_nested_identifier_default(inner: Option<Example>, expect_ok: bool) {
    #[derive(ValidFidlTable, Debug, PartialEq, Eq)]
    #[fidl_table_src(Example)]
    struct Valid {
        num: u32,
    }

    const DEFAULT_VALID: Valid = Valid { num: 42 };

    #[derive(ValidFidlTable, Debug, PartialEq)]
    #[fidl_table_src(WrapExample)]
    struct WrapValid {
        #[fidl_field_with_default(DEFAULT_VALID)]
        inner: Valid,
    }

    let got = WrapValid::try_from(WrapExample { inner, ..Default::default() });
    if expect_ok {
        assert_matches!(
            got,
            Ok(WrapValid { inner }) => assert_eq!(inner, DEFAULT_VALID)
        );
    } else {
        assert_matches!(got, Err(WrapExampleValidationError::InvalidField(_)));
    }
}
