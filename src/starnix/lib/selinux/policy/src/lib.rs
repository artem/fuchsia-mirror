// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod error;
pub mod index;
pub mod metadata;
pub mod parsed_policy;
pub mod parser;

mod arrays;
mod extensible_bitmap;
mod symbols;

use {
    error::{NewSecurityContextError, ParseError, QueryError},
    index::PolicyIndex,
    metadata::HandleUnknown,
    parsed_policy::ParsedPolicy,
    parser::{ByRef, ParseStrategy},
};

use anyhow::Context as _;
use once_cell::sync::Lazy;
use parser::ByValue;
use selinux_common::{
    self as sc, security_context::SecurityContext, ClassPermission as _, FileClass,
};
use std::{collections::BTreeMap, fmt::Debug, marker::PhantomData, ops::Deref};
use zerocopy::{little_endian as le, ByteSlice, FromBytes, NoCell, Ref, Unaligned};

/// Maximum SELinux policy version supported by this implementation.
pub const SUPPORTED_POLICY_VERSION: u32 = 33;

/// The set of permissions that may be granted to sources accessing targets of a particular class,
/// as defined in an SELinux policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessVector(u32);

impl AccessVector {
    pub const NONE: AccessVector = AccessVector(0);
    pub const ALL: AccessVector = AccessVector(std::u32::MAX);

    pub(crate) fn from_raw(access_vector: u32) -> Self {
        Self(access_vector)
    }

    #[cfg(feature = "selinux_policy_test_api")]
    pub fn into_raw(self) -> u32 {
        self.0
    }
}

impl std::ops::BitAnd for AccessVector {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        AccessVector(self.0 & rhs.0)
    }
}

impl std::ops::BitOr for AccessVector {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        AccessVector(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for AccessVector {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0
    }
}

/// Binary policy SIDs that may be referenced in the policy without be explicitly introduced in the
/// policy because they are hard-coded in the Linux kernel.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
pub(crate) static INITIAL_SIDS_IDENTIFIERS: Lazy<BTreeMap<u32, &'static [u8]>> = Lazy::new(|| {
    BTreeMap::<u32, &'static [u8]>::from([
        (1, b"kernel".as_slice()),
        (2, b"security".as_slice()),
        (3, b"unlabeled".as_slice()),
        (4, b"fs".as_slice()),
        (5, b"file".as_slice()),
        (6, b"file_labels".as_slice()),
        (7, b"init".as_slice()),
        (8, b"any_socket".as_slice()),
        (9, b"port".as_slice()),
        (10, b"netif".as_slice()),
        (11, b"netmsg".as_slice()),
        (12, b"node".as_slice()),
        (13, b"igmp_packet".as_slice()),
        (14, b"icmp_socket".as_slice()),
        (15, b"tcp_socket".as_slice()),
        (16, b"sysctl_modprobe".as_slice()),
        (17, b"sysctl".as_slice()),
        (18, b"sysctl_fs".as_slice()),
        (19, b"sysctl_kernel".as_slice()),
        (20, b"sysctl_net".as_slice()),
        (21, b"sysctl_net_unix".as_slice()),
        (22, b"sysctl_vm".as_slice()),
        (23, b"sysctl_dev".as_slice()),
        (24, b"kmod".as_slice()),
        (25, b"policy".as_slice()),
        (26, b"scmp_packet".as_slice()),
        (27, b"devnull".as_slice()),
    ])
});

/// Parses `binary_policy` by value; that is, copies underlying binary data out in addition to
/// building up parser output structures. This function returns
/// `(unvalidated_parser_output, binary_policy)` on success, or an error if parsing failed. Note
/// that the second component of the success case contains precisely the same bytes as the input.
/// This function depends on a uniformity of interface between the "by value" and "by reference"
/// strategies, but also requires an `unvalidated_parser_output` type that is independent of the
/// `binary_policy` lifetime. Taken together, these requirements demand the "move-in + move-out"
/// interface for `binary_policy`.
///
/// If the caller does not need access to the binary policy when parsing fails, but does need to
/// retain both the parsed output and the binary policy when parsing succeeds, the code will look
/// something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, binary_policy) = parse_policy_by_value(binary_policy)?;
/// ```
///
/// If the caller does need access to the binary policy when parsing fails and needs to retain both
/// parsed output and the binary policy when parsing succeeds, the code will look something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, _) = parse_policy_by_value(binary_policy.clone())?;
/// ```
///
/// If the caller does not need to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_reference`] should be used instead.
pub fn parse_policy_by_value(
    binary_policy: Vec<u8>,
) -> Result<(Unvalidated<ByValue<Vec<u8>>>, Vec<u8>), anyhow::Error> {
    let (parsed_policy, binary_policy) =
        ParsedPolicy::parse(ByValue::new(binary_policy)).context("parsing policy")?;
    Ok((Unvalidated(parsed_policy), binary_policy))
}

/// Parses `binary_policy` by reference; that is, constructs parser output structures that contain
/// _references_ to data in `binary_policy`. This function returns `unvalidated_parser_output` on
/// success, or an error if parsing failed.
///
/// If the caller does needs to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_value`] should be used instead.
pub fn parse_policy_by_reference<'a>(
    binary_policy: &'a [u8],
) -> Result<Unvalidated<ByRef<&'a [u8]>>, anyhow::Error> {
    let (parsed_policy, _) =
        ParsedPolicy::parse(ByRef::new(binary_policy)).context("parsing policy")?;
    Ok(Unvalidated(parsed_policy))
}

#[derive(Debug)]
pub struct Policy<PS: ParseStrategy>(PolicyIndex<PS>);

impl<PS: ParseStrategy> Policy<PS> {
    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        self.0.parsed_policy().policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> &HandleUnknown {
        self.0.parsed_policy().handle_unknown()
    }

    pub fn conditional_booleans<'a>(&'a self) -> Vec<(&'a [u8], bool)> {
        self.0
            .parsed_policy()
            .conditional_booleans()
            .iter()
            .map(|boolean| (PS::deref_slice(&boolean.data), PS::deref(&boolean.metadata).active()))
            .collect()
    }

    /// Returns the the security context that should be applied to a newly created file-like SELinux
    /// object according to `source` and `target` security contexts, as well as the new object's
    /// `class`. Returns an error if the security context for such an object is not well-defined
    /// by this [`Policy`].
    pub fn new_file_security_context(
        &self,
        _source: &SecurityContext,
        _target: &SecurityContext,
        _class: &FileClass,
    ) -> Result<SecurityContext, NewSecurityContextError> {
        todo!("Implement computing security contexts for newly created file-like objects");
    }

    /// Returns whether the input types are explicitly granted `permission` via an `allow [...];`
    /// policy statement, or an error if looking up the input types fails.
    pub fn is_explicitly_allowed(
        &self,
        source_type_name: &str,
        target_type_name: &str,
        permission: sc::Permission,
    ) -> Result<bool, QueryError> {
        let object_class = permission.class();
        let target_class = self.0.class(&object_class);
        let permission = self.0.permission(&permission);
        self.0.parsed_policy().class_permission_is_explicitly_allowed(
            source_type_name,
            target_type_name,
            target_class,
            permission,
        )
    }

    /// Returns whether the input types are explicitly granted the permission named
    /// `permission_name` via an `allow [...];` policy statement, or an error if looking up the
    /// input types fails. This is the "custom" form of this API because `permission_name` is
    /// associated with a [`selinux_common::AbstractPermission::Custom::permission`] value.
    pub fn is_explicitly_allowed_custom(
        &self,
        source_type_name: &str,
        target_type_name: &str,
        target_class_name: &str,
        permission_name: &str,
    ) -> Result<bool, QueryError> {
        self.0.parsed_policy().is_explicitly_allowed_custom(
            source_type_name,
            target_type_name,
            target_class_name,
            permission_name,
        )
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists.
    pub fn compute_explicitly_allowed(
        &self,
        source_type_name: &str,
        target_type_name: &str,
        object_class: sc::ObjectClass,
    ) -> Result<AccessVector, QueryError> {
        let target_class = self.0.class(&object_class);
        self.0.parsed_policy().compute_explicitly_allowed(
            source_type_name,
            target_type_name,
            target_class,
        )
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists. This is the "custom" form of this API because
    /// `target_class_name` is associated with a [`selinux_common::AbstractObjectClass::Custom`]
    /// value.
    pub fn compute_explicitly_allowed_custom(
        &self,
        source_type_name: &str,
        target_type_name: &str,
        target_class_name: &str,
    ) -> Result<AccessVector, QueryError> {
        self.0.parsed_policy().compute_explicitly_allowed_custom(
            source_type_name,
            target_type_name,
            target_class_name,
        )
    }

    #[cfg(feature = "selinux_policy_test_api")]
    pub fn print_permissions(&self) {
        let parsed_policy = self.0.parsed_policy();
        for class in parsed_policy.classes().into_iter() {
            println!("{}", std::str::from_utf8(class.name_bytes()).expect("class name"));
            for permission in class.permissions().into_iter() {
                println!(
                    "    {}",
                    std::str::from_utf8(permission.name_bytes()).expect("permission name")
                );
            }
        }
    }
}

impl<PS: ParseStrategy> AccessVectorComputer for Policy<PS> {
    fn access_vector_from_permission<P: sc::ClassPermission + Into<sc::Permission> + 'static>(
        &self,
        permission: P,
    ) -> AccessVector {
        let permission = self.0.permission(&permission.into());

        // Compute bit flag associated with permission.
        // Use `permission.value() - 1` below because values start at `1` to refer to the
        // "shift `1` by 0 bits".
        //
        // value=1 => bits:0...001, value=2 => bits:0...010, etc.
        AccessVector(1 << (permission.value() - 1))
    }

    fn access_vector_from_permissions<
        'a,
        P: sc::ClassPermission + Into<sc::Permission> + 'static,
        PI: IntoIterator<Item = P>,
    >(
        &self,
        permissions: PI,
    ) -> AccessVector {
        let mut access_vector = AccessVector::NONE;
        for permission in permissions.into_iter() {
            access_vector |= self.access_vector_from_permission(permission);
        }
        access_vector
    }
}

impl<PS: ParseStrategy> Validate for Policy<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.0.parsed_policy().validate()
    }
}

/// A [`Policy`] that has been successfully parsed, but not validated.
pub struct Unvalidated<PS: ParseStrategy>(ParsedPolicy<PS>);

impl<PS: ParseStrategy> Unvalidated<PS> {
    pub fn validate(self) -> Result<Policy<PS>, anyhow::Error> {
        Validate::validate(&self.0).context("validating parsed policy")?;
        let index = PolicyIndex::new(self.0).context("building index")?;
        Ok(Policy(index))
    }

    #[cfg(feature = "selinux_policy_test_api")]
    pub fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.0
    }
}

/// An owner of policy information that can translate [`sc::Permission`] values into
/// [`AccessVector`] values that are consistent with the owned policy.
pub trait AccessVectorComputer {
    /// Returns an [`AccessVector`] with a single bit set that corresponds to `permission`.
    fn access_vector_from_permission<P: sc::ClassPermission + Into<sc::Permission> + 'static>(
        &self,
        permission: P,
    ) -> AccessVector;

    /// Computes an [`AccessVector`] where the only bits set are those that correspond to
    /// all `permissions`. This operation fails if `permissions` contain permissions that refer to
    /// different object classes because an access vector specifies permission bits associated with
    /// one specific object class.
    fn access_vector_from_permissions<
        'a,
        P: sc::ClassPermission + Into<sc::Permission> + 'static,
        PI: IntoIterator<Item = P>,
    >(
        &self,
        permissions: PI,
    ) -> AccessVector;
}

/// A data structure that can be parsed as a part of a binary policy.
pub trait Parse<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` from `bytes`, returning the `Self` and trailing bytes, or an error if
    /// bytes corresponding to a `Self` are malformed.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error>;
}

/// Parse a data as a slice of inner data structures from a prefix of a [`ByteSlice`].
pub(crate) trait ParseSlice<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` as `count` of internal itemsfrom `bytes`, returning the `Self` and trailing
    /// bytes, or an error if bytes corresponding to a `Self` are malformed.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error>;
}

/// Validate a parsed data structure.
pub(crate) trait Validate {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate(&self) -> Result<(), Self::Error>;
}

pub(crate) trait ValidateArray<M, D> {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate_array<'a>(metadata: &'a M, data: &'a [D]) -> Result<(), Self::Error>;
}

/// Treat a type as metadata that contains a count of subsequent data.
pub(crate) trait Counted {
    /// Returns the count of subsequent data items.
    fn count(&self) -> u32;
}

impl<T: Validate> Validate for Option<T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        match self {
            Some(value) => value.validate(),
            None => Ok(()),
        }
    }
}

impl Validate for le::U32 {
    type Error = anyhow::Error;

    /// Using a raw `le::U32` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(le::U32);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for u8 {
    type Error = anyhow::Error;

    /// Using a raw `u8` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(u8);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [u8] {
    type Error = anyhow::Error;

    /// Using a raw `[u8]` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T([u8]);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice, T: Validate + FromBytes + NoCell> Validate for Ref<B, T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.deref().validate()
    }
}

impl<B: ByteSlice, T: Counted + FromBytes + NoCell> Counted for Ref<B, T> {
    fn count(&self) -> u32 {
        self.deref().count()
    }
}

/// A length-encoded array that contains metadata in `M` and a slice of data items internally
/// managed by `D`.
#[derive(Clone, Debug, PartialEq)]
struct Array<PS, M, D> {
    metadata: M,
    data: D,
    _marker: PhantomData<PS>,
}

impl<PS: ParseStrategy, M: Counted + Parse<PS>, D: ParseSlice<PS>> Parse<PS> for Array<PS, M, D> {
    /// [`Array`] abstracts over two types (`M` and `D`) that may have different [`Parse::Error`]
    /// types. Unify error return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Array`] by parsing *and validating* `metadata`, `data`, and `self`.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = M::parse(tail).map_err(Into::<anyhow::Error>::into)?;

        let (data, tail) =
            D::parse_slice(tail, metadata.count() as usize).map_err(Into::<anyhow::Error>::into)?;

        let array = Self { metadata, data, _marker: PhantomData };

        Ok((array, tail))
    }
}

impl<
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
        PS: ParseStrategy<Output<T> = T>,
    > Parse<PS> for T
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = PS::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

/// Defines a at type that wraps an [`Array`], implementing `Deref`-as-`Array` and [`Parse`]. This
/// macro should be used in contexts where using a general [`Array`] implementation may introduce
/// conflicting implementations on account of general [`Array`] type parameters.
macro_rules! array_type {
    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty, $metadata_type_name:expr, $data_type_name:expr) => {
        #[doc = "An [`Array`] with [`"]
        #[doc = $metadata_type_name]
        #[doc = "`] metadata and [`"]
        #[doc = $data_type_name]
        #[doc = "`] data items."]
        #[derive(Debug, PartialEq)]
        pub(crate) struct $type_name<$parse_strategy: crate::parser::ParseStrategy>(
            crate::Array<PS, $metadata_type, $data_type>,
        );

        impl<PS: crate::parser::ParseStrategy> std::ops::Deref for $type_name<PS> {
            type Target = crate::Array<PS, $metadata_type, $data_type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<PS: crate::parser::ParseStrategy> crate::Parse<PS> for $type_name<PS>
        where
            Array<PS, $metadata_type, $data_type>: crate::Parse<PS>,
        {
            type Error = <Array<PS, $metadata_type, $data_type> as crate::Parse<PS>>::Error;

            fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
                let (array, tail) = Array::<PS, $metadata_type, $data_type>::parse(bytes)?;
                Ok((Self(array), tail))
            }
        }
    };

    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty) => {
        array_type!(
            $type_name,
            $parse_strategy,
            $metadata_type,
            $data_type,
            stringify!($metadata_type),
            stringify!($data_type)
        );
    };
}

pub(crate) use array_type;

macro_rules! array_type_validate_deref_both {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data).map_err(Into::<anyhow::Error>::into)
            }
        }
    };
}

pub(crate) use array_type_validate_deref_both;

macro_rules! array_type_validate_deref_data {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data)
            }
        }
    };
}

pub(crate) use array_type_validate_deref_data;

macro_rules! array_type_validate_deref_metadata_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(crate) use array_type_validate_deref_metadata_data_vec;

macro_rules! array_type_validate_deref_none_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(crate) use array_type_validate_deref_none_data_vec;

impl<
        B: Debug + ByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    > Parse<ByRef<B>> for Ref<B, T>
{
    type Error = anyhow::Error;

    fn parse(bytes: ByRef<B>) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = ByRef::<B>::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

impl<
        B: Debug + ByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    > ParseSlice<ByRef<B>> for Ref<B, [T]>
{
    /// [`Ref`] may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error return
    /// type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Ref`] by consuming it as an unaligned prefix as a slice, then validating the slice
    /// via `Ref::deref`.
    fn parse_slice(bytes: ByRef<B>, count: usize) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) =
            ByRef::<B>::parse_slice::<T>(bytes, count).ok_or(ParseError::MissingSliceData {
                type_name: std::any::type_name::<T>(),
                type_size: std::mem::size_of::<T>(),
                num_items: count,
                num_bytes,
            })?;

        Ok((data, tail))
    }
}

impl<PS: ParseStrategy, T: Parse<PS>> ParseSlice<PS> for Vec<T> {
    /// `Vec<T>` may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error
    /// return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses `Vec<T>` by parsing individual `T` instances, then validating them.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error> {
        let mut slice = Vec::with_capacity(count);
        let mut tail = bytes;

        for _ in 0..count {
            let (item, next_tail) = T::parse(tail).map_err(Into::<anyhow::Error>::into)?;
            slice.push(item);
            tail = next_tail;
        }

        Ok((slice, tail))
    }
}

pub mod testing {
    use super::AccessVector;

    pub const ACCESS_VECTOR_0001: AccessVector = AccessVector(0b0001u32);
    pub const ACCESS_VECTOR_0010: AccessVector = AccessVector(0b0010u32);
    pub const ACCESS_VECTOR_0100: AccessVector = AccessVector(0b0100u32);
    pub const ACCESS_VECTOR_1000: AccessVector = AccessVector(0b1000u32);
}

#[cfg(test)]
pub(crate) mod test {
    use super::*;

    use super::error::ValidateError;

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub fn as_parse_error(error: anyhow::Error) -> ParseError {
        error.downcast::<ParseError>().expect("parse error")
    }

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub fn as_validate_error(error: anyhow::Error) -> ValidateError {
        error.downcast::<ValidateError>().expect("validate error")
    }

    macro_rules! parse_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = $data;
            fn check_by_ref<'a>(
                $result: Result<
                    ($parse_output<ByRef<&'a [u8]>>, ByRef<&'a [u8]>),
                    <$parse_output<ByRef<&'a [u8]>> as crate::Parse<ByRef<&'a [u8]>>>::Error,
                >,
            ) {
                $check_impl;
            }

            fn check_by_value(
                $result: Result<
                    ($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>),
                    <$parse_output<ByValue<Vec<u8>>> as crate::Parse<ByValue<Vec<u8>>>>::Error,
                >,
            ) -> Option<($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>)> {
                $check_impl
            }

            let by_ref = ByRef::new(data.as_slice());
            let by_ref_result = $parse_output::parse(by_ref);
            check_by_ref(by_ref_result);
            let by_value_result = $parse_output::<ByValue<Vec<u8>>>::parse(ByValue::new(data));
            let _ = check_by_value(by_value_result);
        }};
    }

    pub(crate) use parse_test;

    macro_rules! validate_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = $data;
            fn check_by_ref<'a>(
                $result: Result<(), <$parse_output<ByRef<&'a [u8]>> as crate::Validate>::Error>,
            ) {
                $check_impl;
            }

            fn check_by_value(
                $result: Result<(), <$parse_output<ByValue<Vec<u8>>> as crate::Validate>::Error>,
            ) {
                $check_impl
            }

            let by_ref = ByRef::new(data.as_slice());
            let (by_ref_parsed, _) =
                $parse_output::parse(by_ref).expect("successful parse for validate test");
            let by_ref_result = by_ref_parsed.validate();
            check_by_ref(by_ref_result);
            let (by_value_parsed, _) = $parse_output::<ByValue<Vec<u8>>>::parse(ByValue::new(data))
                .expect("successful parse for validate test");
            let by_value_result = by_value_parsed.validate();
            check_by_value(by_value_result);
        }};
    }

    pub(crate) use validate_test;
}
