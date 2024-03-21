// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    array_type, array_type_validate_deref_both, array_type_validate_deref_data,
    array_type_validate_deref_metadata_data_vec, array_type_validate_deref_none_data_vec,
    error::{ParseError, ValidateError},
    extensible_bitmap::ExtensibleBitmap,
    parser::ParseStrategy,
    Array, CategoryId, Counted, Parse, ParseSlice, RoleId, SensitivityId, TypeId, UserId, Validate,
    ValidateArray,
};

use anyhow::Context as _;
use std::{fmt::Debug, num::NonZeroU32, ops::Deref};
use zerocopy::{little_endian as le, FromBytes, FromZeroes, NoCell, Unaligned};

/// The `type` field value for a [`Constraint`] that contains an [`ExtensibleBitmap`] and
/// [`TypeSet`].
const CONSTRAINT_TYPE_HAS_EXTENSIBLE_BITMAP_AND_TYPE_SET: u32 = 5;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux type.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
pub(crate) const TYPE_PROPERTIES_TYPE: u32 = 1;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux alias.
pub(crate) const TYPE_PROPERTIES_ALIAS: u32 = 0;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux attribute.
pub(crate) const TYPE_PROPERTIES_ATTRIBUTE: u32 = 0;

/// [`SymbolList`] is an [`Array`] of items with the count of items determined by [`Metadata`] as
/// [`Counted`].
#[derive(Debug, PartialEq)]
pub(crate) struct SymbolList<PS: ParseStrategy, T>(Array<PS, PS::Output<Metadata>, Vec<T>>);

impl<PS: ParseStrategy, T> Deref for SymbolList<PS, T> {
    type Target = Array<PS, PS::Output<Metadata>, Vec<T>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<PS: ParseStrategy, T> Parse<PS> for SymbolList<PS, T>
where
    Array<PS, PS::Output<Metadata>, Vec<T>>: Parse<PS>,
{
    type Error = <Array<PS, PS::Output<Metadata>, Vec<T>> as Parse<PS>>::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let (array, tail) = Array::<PS, PS::Output<Metadata>, Vec<T>>::parse(bytes)?;
        Ok((Self(array), tail))
    }
}

impl<PS: ParseStrategy, T> Validate for SymbolList<PS, T>
where
    [T]: Validate,
{
    type Error = anyhow::Error;

    /// [`SymbolList`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        PS::deref(&self.metadata).validate().map_err(Into::<anyhow::Error>::into)?;
        self.data.as_slice().validate().map_err(Into::<anyhow::Error>::into)?;

        Ok(())
    }
}

/// Binary metadata prefix to [`SymbolList`] objects.
#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Metadata {
    /// The number of primary names referred to in the associated [`SymbolList`].
    primary_names_count: le::U32,
    /// The number of objects in the associated [`SymbolList`] [`Array`].
    count: le::U32,
}

impl Metadata {
    pub fn primary_names_count(&self) -> u32 {
        self.primary_names_count.get()
    }
}

impl Counted for Metadata {
    /// The number of items that follow a [`Metadata`] is the value stored in the `metadata.count`
    /// field.
    fn count(&self) -> u32 {
        self.count.get()
    }
}

impl Validate for Metadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `primary_names_count` or `count`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [CommonSymbol<PS>] {
    type Error = <CommonSymbol<PS> as Validate>::Error;

    /// [`CommonSymbols`] have no internal constraints beyond those imposed by individual
    /// [`CommonSymbol`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbol, PS, CommonSymbolMetadata<PS>, Permissions<PS>);

array_type_validate_deref_none_data_vec!(CommonSymbol);

impl<PS: ParseStrategy> CommonSymbol<PS> {
    pub fn permissions(&self) -> &Permissions<PS> {
        &self.data
    }
}

pub(crate) type CommonSymbols<PS> = Vec<CommonSymbol<PS>>;

impl<PS: ParseStrategy> CommonSymbol<PS> {
    /// Returns the name of this common symbol (a string), encoded a borrow of a byte slice. For
    /// example, the policy statement `common file { common_file_perm }` induces a [`CommonSymbol`]
    /// where `name_bytes() == "file".as_slice()`.
    pub fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.metadata.data)
    }
}

impl<PS: ParseStrategy> Counted for CommonSymbol<PS>
where
    CommonSymbolMetadata<PS>: Parse<PS> + Validate,
    Array<PS, PS::Output<CommonSymbolStaticMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<PermissionMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, CommonSymbolMetadata<PS>, Vec<Permission<PS>>>: Parse<PS>,
    Vec<Permission<PS>>: ParseSlice<PS>,
{
    /// The count of items in the associated [`Permissions`] is exposed via
    /// `CommonSymbolMetadata::count()`.
    fn count(&self) -> u32 {
        self.metadata.count()
    }
}

impl<PS: ParseStrategy> ValidateArray<CommonSymbolMetadata<PS>, Permission<PS>>
    for CommonSymbol<PS>
{
    type Error = anyhow::Error;

    /// [`CommonSymbol`] have no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a CommonSymbolMetadata<PS>,
        _data: &'a [Permission<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbolMetadata, PS, PS::Output<CommonSymbolStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(CommonSymbolMetadata);

impl<PS: ParseStrategy> Counted for CommonSymbolMetadata<PS> {
    /// The count of items in the associated [`Permissions`] is stored in the associated
    /// `CommonSymbolStaticMetadata::count` field.
    fn count(&self) -> u32 {
        PS::deref(&self.metadata).count.get()
    }
}

impl<PS: ParseStrategy> ValidateArray<CommonSymbolStaticMetadata, u8> for CommonSymbolMetadata<PS> {
    type Error = anyhow::Error;

    /// Array of [`u8`] sized by [`CommonSymbolStaticMetadata`] requires no additional validation.
    fn validate_array<'a>(
        _metadata: &'a CommonSymbolStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Static (that is, fixed-sized) metadata for a common symbol.
#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct CommonSymbolStaticMetadata {
    /// The length of the `[u8]` key stored in the associated [`CommonSymbolMetadata`].
    length: le::U32,
    /// An integer that identifies this this common symbol, unique to this common symbol relative
    /// to all common symbols and classes in this policy.
    id: le::U32,
    /// The number of primary names referred to by the associated [`CommonSymbol`].
    primary_names_count: le::U32,
    /// The number of items stored in the [`Permissions`] in the associated [`CommonSymbol`].
    count: le::U32,
}

impl Validate for CommonSymbolStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Counted for CommonSymbolStaticMetadata {
    /// The count of bytes in the `[u8]` in the associated [`CommonSymbolMetadata`].
    fn count(&self) -> u32 {
        self.length.get()
    }
}

/// [`Permissions`] is a dynamically allocated slice (that is, [`Vec`]) of [`Permission`].
pub(crate) type Permissions<PS> = Vec<Permission<PS>>;

impl<PS: ParseStrategy> Validate for Permissions<PS> {
    type Error = anyhow::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Permission, PS, PS::Output<PermissionMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Permission);

impl<PS: ParseStrategy> Permission<PS> {
    /// Returns the name of this permission (a string), encoded a borrow of a byte slice. For
    /// example the class named `"file"` class has a permission named `"entrypoint"` and the
    /// `"process"` class has a permission named `"fork"`.
    pub fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.data)
    }

    pub fn id(&self) -> u32 {
        PS::deref(&self.metadata).id.get()
    }
}

impl<PS: ParseStrategy> ValidateArray<PermissionMetadata, u8> for Permission<PS> {
    type Error = anyhow::Error;

    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a PermissionMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct PermissionMetadata {
    /// The length of the `[u8]` in the associated [`Permission`].
    length: le::U32,
    id: le::U32,
}

impl Counted for PermissionMetadata {
    /// The count of bytes in the `[u8]` in the associated [`Permission`].
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for PermissionMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConstraintsList<PS> = Vec<PermissionAndConstraints<PS>>;

impl<PS: ParseStrategy> Validate for ConstraintsList<PS> {
    type Error = anyhow::Error;

    /// [`ConstraintsList`] have no internal constraints beyond those imposed by individual
    /// [`PermissionAndConstraints`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct PermissionAndConstraints<PS: ParseStrategy>
where
    ConstraintList<PS>: Debug + PartialEq,
{
    permission_bitset: PS::Output<le::U32>,
    constraints: ConstraintList<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for PermissionAndConstraints<PS>
where
    ConstraintList<PS>: Debug + PartialEq + Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (permission_bitset, tail) = PS::parse::<le::U32>(tail).ok_or(
            Into::<anyhow::Error>::into(ParseError::MissingData {
                type_name: "PermissionBitset",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            }),
        )?;
        let (constraints, tail) = ConstraintList::parse(tail)
            .map_err(|error| error.into() as anyhow::Error)
            .context("parsing constraint list in constraints list")?;

        Ok((Self { permission_bitset, constraints }, tail))
    }
}

array_type!(ConstraintList, PS, PS::Output<ConstraintCount>, Constraints<PS>);

array_type_validate_deref_metadata_data_vec!(ConstraintList);

impl<PS: ParseStrategy> ValidateArray<ConstraintCount, Constraint<PS>> for ConstraintList<PS> {
    type Error = anyhow::Error;

    /// [`ConstraintList`] has no internal constraints beyond those imposed by [`Array`].
    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ConstraintCount,
        _data: &'a [Constraint<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintCount(le::U32);

impl Counted for ConstraintCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ConstraintCount {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on constraint count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for Constraints<PS> {
    type Error = anyhow::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Constraint<PS: ParseStrategy> {
    metadata: PS::Output<ConstraintMetadata>,
    names: Option<ExtensibleBitmap<PS>>,
    names_type_set: Option<TypeSet<PS>>,
}

pub(crate) type Constraints<PS> = Vec<Constraint<PS>>;

impl<PS: ParseStrategy> Parse<PS> for Constraint<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PS::parse::<ConstraintMetadata>(tail).context("parsing constraint metadata")?;

        let (names, names_type_set, tail) = match PS::deref(&metadata).constraint_type.get() {
            CONSTRAINT_TYPE_HAS_EXTENSIBLE_BITMAP_AND_TYPE_SET => {
                let (names, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing constraint names")?;
                let (names_type_set, tail) =
                    TypeSet::parse(tail).context("parsing constraint names type set")?;
                (Some(names), Some(names_type_set), tail)
            }
            _ => (None, None, tail),
        };

        Ok((Self { metadata, names, names_type_set }, tail))
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintMetadata {
    constraint_type: le::U32,
    attribute: le::U32,
    operands: le::U32,
}

impl Validate for ConstraintMetadata {
    type Error = anyhow::Error;

    /// TODO: Verify meaningful encoding of `constraint_type`, `attribute`, `operands`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct TypeSet<PS: ParseStrategy> {
    types: ExtensibleBitmap<PS>,
    negative_set: ExtensibleBitmap<PS>,
    flags: PS::Output<le::U32>,
}

impl<PS: ParseStrategy> Parse<PS> for TypeSet<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (types, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set types")?;

        let (negative_set, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set negative set")?;

        let num_bytes = tail.len();
        let (flags, tail) = PS::parse::<le::U32>(tail).ok_or(Into::<anyhow::Error>::into(
            ParseError::MissingData {
                type_name: "TypeSetFlags",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            },
        ))?;

        Ok((Self { types, negative_set, flags }, tail))
    }
}

/// Locates a class named `name` among `classes`. Returns the first such class found, though policy
/// validation should ensure that only one such class exists.
pub(crate) fn find_class_by_name<'a, PS: ParseStrategy>(
    classes: &'a Classes<PS>,
    name: &str,
) -> Option<&'a Class<PS>> {
    find_class_by_name_bytes(classes, name.as_bytes())
}

fn find_class_by_name_bytes<'a, PS: ParseStrategy>(
    classes: &'a Classes<PS>,
    name_bytes: &[u8],
) -> Option<&'a Class<PS>> {
    for cls in classes.into_iter() {
        if cls.name_bytes() == name_bytes {
            return Some(cls);
        }
    }

    None
}

/// Locates a class named `name` among `classes`. Returns the first such class found, though policy
/// validation should ensure that only one such class exists.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
pub(crate) fn find_common_symbol_by_name<'a, PS: ParseStrategy>(
    common_symbols: &'a CommonSymbols<PS>,
    name: &str,
) -> Option<&'a CommonSymbol<PS>> {
    find_common_symbol_by_name_bytes(common_symbols, name.as_bytes())
}

fn find_common_symbol_by_name_bytes<'a, PS: ParseStrategy>(
    common_symbols: &'a CommonSymbols<PS>,
    name_bytes: &[u8],
) -> Option<&'a CommonSymbol<PS>> {
    for common_symbol in common_symbols.into_iter() {
        if common_symbol.name_bytes() == name_bytes {
            return Some(common_symbol);
        }
    }

    None
}

/// Locates a permission named `name` associated with `class`. The policy language supports a
/// limited form of inheritance. For example:
///
/// ```config
/// common file { ioctl read write create [...] }
/// class file inherits file { execute_no_trans entrypoint }
/// ```
///
/// Each `class` may inherit from zero or one `common`, in which case the permissions specified in
/// the denoted `common` are also permissions in `class`. Locating these "common" permissions is the
/// reason that `common_symbols` is received as a function input.
pub(crate) fn find_class_permission_by_name<'a, PS: ParseStrategy>(
    common_symbols: &'a CommonSymbols<PS>,
    class: &'a Class<PS>,
    name: &'a str,
) -> Option<&'a Permission<PS>> {
    let name_bytes = name.as_bytes();
    if let Some(permission) = find_own_class_permission_by_name_bytes(class, name_bytes) {
        Some(permission)
    } else if let Some(common_symbol) =
        find_common_symbol_by_name_bytes(common_symbols, class.common_name_bytes())
    {
        find_common_permission_by_name_bytes(common_symbol, name_bytes)
    } else {
        None
    }
}

fn find_own_class_permission_by_name_bytes<'a, PS: ParseStrategy>(
    class: &'a Class<PS>,
    name_bytes: &'a [u8],
) -> Option<&'a Permission<PS>> {
    let own_permissions: &Permissions<PS> = &class.constraints.metadata.data;
    find_own_permission_by_name_bytes(own_permissions, name_bytes)
}

fn find_common_permission_by_name_bytes<'a, PS: ParseStrategy>(
    common_symbol: &'a CommonSymbol<PS>,
    name_bytes: &'a [u8],
) -> Option<&'a Permission<PS>> {
    let own_permissions: &Permissions<PS> = &common_symbol.data;
    find_own_permission_by_name_bytes(own_permissions, name_bytes)
}

fn find_own_permission_by_name_bytes<'a, PS: ParseStrategy>(
    own_permissions: &'a Permissions<PS>,
    name_bytes: &'a [u8],
) -> Option<&'a Permission<PS>> {
    for permission in own_permissions {
        if permission.name_bytes() == name_bytes {
            return Some(permission);
        }
    }

    None
}

impl<PS: ParseStrategy> Validate for [Class<PS>] {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        // TODO: Validate internal consistency between consecutive [`Class`] instances.
        for class in self {
            // TODO: Validate `self.constraints` and `self.validate_transitions`.
            class.defaults().validate().context("class defaults")?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Class<PS: ParseStrategy> {
    constraints: ClassConstraints<PS>,
    validate_transitions: ClassValidateTransitions<PS>,
    defaults: PS::Output<ClassDefaults>,
}

pub(crate) type Classes<PS> = Vec<Class<PS>>;

impl<PS: ParseStrategy> Class<PS> {
    /// Returns the name of the `common` from which this `class` inherits as a borrow of a byte
    /// slice. For example, `common file { common_file_perm }`,
    /// `class file inherits file { file_perm }` yields two [`Class`] objects, one that refers to a
    /// permission named `"common_file_perm"` permission and has `self.common_name_bytes() == ""`,
    /// and another that refers to a permission named `"file_perm"` and has
    /// `self.common_name_bytes() == "file"`.
    pub fn common_name_bytes(&self) -> &[u8] {
        // `ClassCommonKey` is an `Array` of `[u8]` with metadata `ClassKey`, and
        // `ClassKey::count()` returns the `common_key_length` field. That is, the `[u8]` string
        // on `ClassCommonKey` is the "common key" (name in the inherited `common` statement) for
        // this class.
        let class_common_key: &ClassCommonKey<PS> = &self.constraints.metadata.metadata;
        PS::deref_slice(&class_common_key.data)
    }

    /// Returns the name of this class as a borrow of a byte slice.
    pub fn name_bytes(&self) -> &[u8] {
        // `ClassKey` is an `Array` of `[u8]` with metadata `ClassMetadata`, and
        // `ClassMetadata::count()` returns the `key_length` field. That is, the `[u8]` string on
        // `ClassKey` is the "class key" (name in the `class` or `common` statement) for this class.
        let class_key: &ClassKey<PS> = &self.constraints.metadata.metadata.metadata;
        PS::deref_slice(&class_key.data)
    }

    /// Returns the id associated with this class. The id is used to index into collections
    /// and bitmaps associated with this class. The id is 1-indexed, whereas most collections and
    /// bitmaps are 0-indexed, so clients of this API will usually use `id - 1`.
    pub fn id(&self) -> le::U32 {
        let class_metadata: &ClassMetadata =
            &PS::deref(&self.constraints.metadata.metadata.metadata.metadata);
        class_metadata.id
    }

    /// Returns whether this class inherits from a named `common` policy statement. For example,
    /// `common file { common_file_perm }`, `class file inherits file { file_perm }` yields two
    /// [`Class`] objects, one that refers to a permission named `"common_file_perm"` permission and
    /// has `self.has_common() == false`, and another that refers to a permission named
    /// `"file_perm"` and has `self.has_common() == true`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn has_common(&self) -> bool {
        self.common_name_bytes().len() != 0
    }

    /// Returns the full listing of permissions used in this policy.
    pub fn permissions(&self) -> &Permissions<PS> {
        &self.constraints.metadata.data
    }

    pub fn defaults(&self) -> &ClassDefaults {
        PS::deref(&self.defaults)
    }
}

impl<PS: ParseStrategy> Parse<PS> for Class<PS>
where
    ClassConstraints<PS>: Parse<PS>,
    ClassValidateTransitions<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (constraints, tail) = ClassConstraints::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class constraints")?;

        let (validate_transitions, tail) = ClassValidateTransitions::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class validate transitions")?;

        let (defaults, tail) =
            PS::parse::<ClassDefaults>(tail).context("parsing class defaults")?;

        Ok((Self { constraints, validate_transitions, defaults }, tail))
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassDefaults {
    default_user: le::U32,
    default_role: le::U32,
    default_range: le::U32,
    default_type: le::U32,
}

impl ClassDefaults {
    pub fn user(&self) -> ClassDefault {
        self.default_user.get().into()
    }

    pub fn role(&self) -> ClassDefault {
        self.default_role.get().into()
    }

    pub fn range(&self) -> ClassDefaultRange {
        self.default_range.get().into()
    }

    pub fn type_(&self) -> ClassDefault {
        self.default_type.get().into()
    }
}

impl Validate for ClassDefaults {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        ClassDefault::validate(self.default_user.get()).context("default user")?;
        ClassDefault::validate(self.default_role.get()).context("default role")?;
        ClassDefault::validate(self.default_type.get()).context("default type")?;
        ClassDefaultRange::validate(self.default_range.get()).context("default range")?;
        Ok(())
    }
}

#[derive(PartialEq)]
pub(crate) enum ClassDefault {
    Unspecified,
    Source,
    Target,
}

impl ClassDefault {
    pub(crate) const DEFAULT_UNSPECIFIED: u32 = 0;
    pub(crate) const DEFAULT_SOURCE: u32 = 1;
    pub(crate) const DEFAULT_TARGET: u32 = 2;

    fn validate(value: u32) -> Result<(), ValidateError> {
        match value {
            Self::DEFAULT_UNSPECIFIED | Self::DEFAULT_SOURCE | Self::DEFAULT_TARGET => Ok(()),
            value => Err(ValidateError::InvalidClassDefault { value }),
        }
    }
}

impl From<u32> for ClassDefault {
    fn from(value: u32) -> Self {
        match value {
            Self::DEFAULT_UNSPECIFIED => Self::Unspecified,
            Self::DEFAULT_SOURCE => Self::Source,
            Self::DEFAULT_TARGET => Self::Target,
            v => panic!(
                "invalid SELinux class default; expected {}, {}, or {}, but got {}",
                Self::DEFAULT_UNSPECIFIED,
                Self::DEFAULT_SOURCE,
                Self::DEFAULT_TARGET,
                v
            ),
        }
    }
}

#[derive(PartialEq)]
pub(crate) enum ClassDefaultRange {
    Unspecified,
    SourceLow,
    SourceHigh,
    SourceLowHigh,
    TargetLow,
    TargetHigh,
    TargetLowHigh,
}

impl ClassDefaultRange {
    pub(crate) const DEFAULT_UNSPECIFIED: u32 = 0;
    pub(crate) const DEFAULT_SOURCE_LOW: u32 = 1;
    pub(crate) const DEFAULT_SOURCE_HIGH: u32 = 2;
    pub(crate) const DEFAULT_SOURCE_LOW_HIGH: u32 = 3;
    pub(crate) const DEFAULT_TARGET_LOW: u32 = 4;
    pub(crate) const DEFAULT_TARGET_HIGH: u32 = 5;
    pub(crate) const DEFAULT_TARGET_LOW_HIGH: u32 = 6;
    // TODO: Determine what this value means.
    pub(crate) const DEFAULT_UNKNOWN_USED_VALUE: u32 = 7;

    fn validate(value: u32) -> Result<(), ValidateError> {
        match value {
            Self::DEFAULT_UNSPECIFIED
            | Self::DEFAULT_SOURCE_LOW
            | Self::DEFAULT_SOURCE_HIGH
            | Self::DEFAULT_SOURCE_LOW_HIGH
            | Self::DEFAULT_TARGET_LOW
            | Self::DEFAULT_TARGET_HIGH
            | Self::DEFAULT_TARGET_LOW_HIGH
            | Self::DEFAULT_UNKNOWN_USED_VALUE => Ok(()),
            value => Err(ValidateError::InvalidClassDefaultRange { value }),
        }
    }
}

impl From<u32> for ClassDefaultRange {
    fn from(value: u32) -> Self {
        match value {
            Self::DEFAULT_UNSPECIFIED => Self::Unspecified,
            Self::DEFAULT_SOURCE_LOW => Self::SourceLow,
            Self::DEFAULT_SOURCE_HIGH => Self::SourceHigh,
            Self::DEFAULT_SOURCE_LOW_HIGH => Self::SourceLowHigh,
            Self::DEFAULT_TARGET_LOW => Self::TargetLow,
            Self::DEFAULT_TARGET_HIGH => Self::TargetHigh,
            Self::DEFAULT_TARGET_LOW_HIGH => Self::TargetLowHigh,
            v => panic!(
                "invalid SELinux MLS range default; expected one of {:?}, but got {}",
                [
                    Self::DEFAULT_UNSPECIFIED,
                    Self::DEFAULT_SOURCE_LOW,
                    Self::DEFAULT_SOURCE_HIGH,
                    Self::DEFAULT_SOURCE_LOW_HIGH,
                    Self::DEFAULT_TARGET_LOW,
                    Self::DEFAULT_TARGET_HIGH,
                    Self::DEFAULT_TARGET_LOW_HIGH,
                ],
                v
            ),
        }
    }
}

array_type!(
    ClassValidateTransitions,
    PS,
    PS::Output<ClassValidateTransitionsCount>,
    Constraints<PS>
);

array_type_validate_deref_metadata_data_vec!(ClassValidateTransitions);

impl<PS: ParseStrategy> ValidateArray<ClassValidateTransitionsCount, Constraint<PS>>
    for ClassValidateTransitions<PS>
{
    type Error = anyhow::Error;

    /// [`ClassValidateTransitions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassValidateTransitionsCount,
        _data: &'a [Constraint<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassValidateTransitionsCount(le::U32);

impl Counted for ClassValidateTransitionsCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ClassValidateTransitionsCount {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on class validate transitions count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassConstraints, PS, ClassPermissions<PS>, ConstraintsList<PS>);

array_type_validate_deref_none_data_vec!(ClassConstraints);

impl<PS: ParseStrategy> ValidateArray<ClassPermissions<PS>, PermissionAndConstraints<PS>>
    for ClassConstraints<PS>
{
    type Error = anyhow::Error;

    /// [`ClassConstraints`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassPermissions<PS>,
        _data: &'a [PermissionAndConstraints<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassPermissions, PS, ClassCommonKey<PS>, Permissions<PS>);

array_type_validate_deref_none_data_vec!(ClassPermissions);

impl<PS: ParseStrategy> ValidateArray<ClassCommonKey<PS>, Permission<PS>> for ClassPermissions<PS> {
    type Error = anyhow::Error;

    /// [`ClassPermissions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassCommonKey<PS>,
        _data: &'a [Permission<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassPermissions<PS>
where
    ClassCommonKey<PS>: Parse<PS>,
    Array<PS, ClassKey<PS>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
    ClassKey<PS>: Parse<PS>,
    Vec<Permission<PS>>: ParseSlice<PS>,
    Array<PS, PS::Output<PermissionMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, ClassCommonKey<PS>, Vec<Permission<PS>>>: Parse<PS>,
{
    /// [`ClassPermissions`] acts as counted metadata for [`ClassConstraints`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata.metadata.metadata).constraint_count.get()
    }
}

array_type!(ClassCommonKey, PS, ClassKey<PS>, PS::Slice<u8>);

array_type_validate_deref_data!(ClassCommonKey);

impl<PS: ParseStrategy> ValidateArray<ClassKey<PS>, u8> for ClassCommonKey<PS> {
    type Error = anyhow::Error;

    /// [`ClassCommonKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(_metadata: &'a ClassKey<PS>, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassCommonKey<PS>
where
    Array<PS, ClassKey<PS>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
    ClassKey<PS>: Parse<PS>,
{
    /// [`ClassCommonKey`] acts as counted metadata for [`ClassPermissions`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata.metadata).elements_count.get()
    }
}

array_type!(ClassKey, PS, PS::Output<ClassMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(ClassKey);

impl<PS: ParseStrategy> ValidateArray<ClassMetadata, u8> for ClassKey<PS> {
    type Error = anyhow::Error;

    /// [`ClassKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassKey<PS>
where
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
{
    /// [`ClassKey`] acts as counted metadata for [`ClassCommonKey`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata).common_key_length.get()
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassMetadata {
    key_length: le::U32,
    common_key_length: le::U32,
    id: le::U32,
    primary_names_count: le::U32,
    elements_count: le::U32,
    constraint_count: le::U32,
}

impl Counted for ClassMetadata {
    fn count(&self) -> u32 {
        self.key_length.get()
    }
}

impl Validate for ClassMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound `u32` values in [`ClassMetadata`]?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Role<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Role`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Role<PS: ParseStrategy> {
    metadata: RoleMetadata<PS>,
    role_dominates: ExtensibleBitmap<PS>,
    role_types: ExtensibleBitmap<PS>,
}

impl<PS: ParseStrategy> Role<PS> {
    pub(crate) fn id(&self) -> RoleId {
        RoleId(NonZeroU32::new(PS::deref(&self.metadata.metadata).id.get()).unwrap())
    }

    pub(crate) fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.metadata.data)
    }
}

impl<PS: ParseStrategy> Parse<PS> for Role<PS>
where
    RoleMetadata<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = RoleMetadata::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role metadata")?;

        let (role_dominates, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role dominates")?;

        let (role_types, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role types")?;

        Ok((Self { metadata, role_dominates, role_types }, tail))
    }
}

array_type!(RoleMetadata, PS, PS::Output<RoleStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(RoleMetadata);

impl<PS: ParseStrategy> ValidateArray<RoleStaticMetadata, u8> for RoleMetadata<PS> {
    type Error = anyhow::Error;

    /// [`RoleMetadata`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a RoleStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct RoleStaticMetadata {
    length: le::U32,
    id: le::U32,
    bounds: le::U32,
}

impl Counted for RoleStaticMetadata {
    /// [`RoleStaticMetadata`] serves as [`Counted`] for a length-encoded `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for RoleStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be any constraints on `length`, `value`, or `bounds`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Returns whether `ty` is associated with `attr` via the mappings `attribute_maps`. Such
/// associations arise from policy statements of the form `typeattribute [ty] [attributes];` where
/// `attr` appears in the comma-separated list, `[attributes]`.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
pub(crate) fn type_has_attribute<'a, PS: ParseStrategy>(
    ty: &'a Type<PS>,
    attr: &'a Type<PS>,
    attribute_maps: &Vec<ExtensibleBitmap<PS>>,
) -> bool {
    let type_id = PS::deref(&ty.metadata).id.get();
    let type_index = type_id - 1;

    let attribute_id = PS::deref(&attr.metadata).id.get();
    let attribute_index = attribute_id - 1;

    attribute_maps[type_index as usize].is_set(attribute_index)
}

impl<PS: ParseStrategy> Validate for [Type<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Type`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Type, PS, PS::Output<TypeMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Type);

impl<PS: ParseStrategy> Type<PS> {
    /// Returns the name of this type.
    pub fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.data)
    }

    /// Returns the id associated with this type. The id is used to index into collections and
    /// bitmaps associated with this type. The id is 1-indexed, whereas most collections and
    /// bitmaps are 0-indexed, so clients of this API will usually use `id - 1`.
    pub fn id(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).id.get()).unwrap())
    }

    /// Returns whether this type is from a `type [name];` policy statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_type(&self) -> bool {
        PS::deref(&self.metadata).properties.get() == TYPE_PROPERTIES_TYPE
    }

    /// Returns whether this type is from a `typealias [typename] alias [aliasname];` policy
    /// statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_alias(&self) -> bool {
        PS::deref(&self.metadata).properties.get() == TYPE_PROPERTIES_ALIAS
    }

    /// Returns whether this type is from an `attribute [name];` policy statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_attribute(&self) -> bool {
        PS::deref(&self.metadata).properties.get() == TYPE_PROPERTIES_ATTRIBUTE
    }
}

impl<PS: ParseStrategy> ValidateArray<TypeMetadata, u8> for Type<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate that `PS::deref(&self.data)` is an ascii string that contains a valid type name.
    fn validate_array<'a>(_metadata: &'a TypeMetadata, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct TypeMetadata {
    length: le::U32,
    id: le::U32,
    properties: le::U32,
    bounds: le::U32,
}

impl Counted for TypeMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for TypeMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`TypeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [User<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`User`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct User<PS: ParseStrategy> {
    user_data: UserData<PS>,
    roles: ExtensibleBitmap<PS>,
    expanded_range: MlsRange<PS>,
    default_level: MlsLevel<PS>,
}

impl<PS: ParseStrategy> User<PS> {
    pub(crate) fn id(&self) -> UserId {
        UserId(NonZeroU32::new(PS::deref(&self.user_data.metadata).id.get()).unwrap())
    }

    pub(crate) fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.user_data.data)
    }
}

impl<PS: ParseStrategy> Parse<PS> for User<PS>
where
    UserData<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (user_data, tail) = UserData::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user data")?;

        let (roles, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user roles")?;

        let (expanded_range, tail) =
            MlsRange::parse(tail).context("parsing user expanded range")?;

        let (default_level, tail) = MlsLevel::parse(tail).context("parsing user default level")?;

        Ok((Self { user_data, roles, expanded_range, default_level }, tail))
    }
}

array_type!(UserData, PS, PS::Output<UserMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(UserData);

impl<PS: ParseStrategy> ValidateArray<UserMetadata, u8> for UserData<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`UserMetadata`] in `self.metadata` and `[u8]` key in `self.data`.
    fn validate_array<'a>(_metadata: &'a UserMetadata, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct UserMetadata {
    length: le::U32,
    id: le::U32,
    bounds: le::U32,
}

impl Counted for UserMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for UserMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`UserMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MlsLevel<PS: ParseStrategy> {
    sensitivity: PS::Output<le::U32>,
    categories: ExtensibleBitmap<PS>,
}

impl<PS: ParseStrategy> MlsLevel<PS> {
    pub fn sensitivity(&self) -> SensitivityId {
        SensitivityId(NonZeroU32::new(PS::deref(&self.sensitivity).get()).unwrap())
    }
    pub fn categories(&self) -> &ExtensibleBitmap<PS> {
        &self.categories
    }
}

impl<PS: ParseStrategy> Parse<PS> for MlsLevel<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (sensitivity, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "MlsLevelSensitivity",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let (categories, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing mls level categories")?;

        Ok((Self { sensitivity, categories }, tail))
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MlsRange<PS: ParseStrategy> {
    count: PS::Output<le::U32>,
    low: MlsLevel<PS>,
    high: Option<MlsLevel<PS>>,
}

impl<PS: ParseStrategy> MlsRange<PS> {
    pub fn low(&self) -> &MlsLevel<PS> {
        &self.low
    }

    pub fn high(&self) -> &Option<MlsLevel<PS>> {
        &self.high
    }
}

impl<PS: ParseStrategy> Parse<PS> for MlsRange<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (count, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "MlsLevelCount",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        // `MlsRange::parse()` cannot be implemented in terms of `MlsLevel::parse()` for the
        // low and optional high level, because of the order in which the sensitivity and
        // category bitmap fields appear.
        let num_bytes = tail.len();
        let (sensitivity_low, tail) =
            PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "MlsLevelSensitivityLow",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (low_categories, high_level, tail) = if PS::deref(&count).get() > 1 {
            let num_bytes = tail.len();
            let (sensitivity_high, tail) =
                PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                    type_name: "MlsLevelSensitivityHigh",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                })?;
            let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range low categories")?;
            let (high_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range high categories")?;

            (
                low_categories,
                Some(MlsLevel { sensitivity: sensitivity_high, categories: high_categories }),
                tail,
            )
        } else {
            let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range low categories")?;

            (low_categories, None, tail)
        };

        Ok((
            Self {
                count,
                low: MlsLevel { sensitivity: sensitivity_low, categories: low_categories },
                high: high_level,
            },
            tail,
        ))
    }
}

impl<PS: ParseStrategy> Validate for [ConditionalBoolean<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`ConditionalBoolean`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ConditionalBoolean, PS, PS::Output<ConditionalBooleanMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(ConditionalBoolean);

impl<PS: ParseStrategy> ValidateArray<ConditionalBooleanMetadata, u8> for ConditionalBoolean<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`ConditionalBooleanMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a ConditionalBooleanMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConditionalBooleanMetadata {
    id: le::U32,
    /// Current active value of this conditional boolean.
    active: le::U32,
    length: le::U32,
}

impl ConditionalBooleanMetadata {
    /// Returns the active value for the boolean.
    pub(crate) fn active(&self) -> bool {
        self.active != le::U32::ZERO
    }
}

impl Counted for ConditionalBooleanMetadata {
    /// [`ConditionalBooleanMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for ConditionalBooleanMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`ConditionalBooleanMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Sensitivity<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Sensitivity`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Sensitivity<PS: ParseStrategy> {
    metadata: SensitivityMetadata<PS>,
    level: MlsLevel<PS>,
}

impl<PS: ParseStrategy> Sensitivity<PS> {
    pub fn id(&self) -> SensitivityId {
        SensitivityId(NonZeroU32::new(PS::deref(&self.level.sensitivity).get()).unwrap())
    }

    pub fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.metadata.data)
    }
}

impl<PS: ParseStrategy> Parse<PS> for Sensitivity<PS>
where
    SensitivityMetadata<PS>: Parse<PS>,
    MlsLevel<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = SensitivityMetadata::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity metadata")?;

        let (level, tail) = MlsLevel::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity mls level")?;

        Ok((Self { metadata, level }, tail))
    }
}

impl<PS: ParseStrategy> Validate for Sensitivity<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of `self.metadata` and `self.level`.
    fn validate(&self) -> Result<(), Self::Error> {
        NonZeroU32::new(PS::deref(&self.level.sensitivity).get())
            .ok_or(ValidateError::NonOptionalIdIsZero)?;
        Ok(())
    }
}

array_type!(SensitivityMetadata, PS, PS::Output<SensitivityStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(SensitivityMetadata);

impl<PS: ParseStrategy> ValidateArray<SensitivityStaticMetadata, u8> for SensitivityMetadata<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`SensitivityMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a SensitivityStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SensitivityStaticMetadata {
    length: le::U32,
    is_alias: le::U32,
}

impl Counted for SensitivityStaticMetadata {
    /// [`SensitivityStaticMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for SensitivityStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`SensitivityStaticMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Category<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Category`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Category, PS, PS::Output<CategoryMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Category);

impl<PS: ParseStrategy> Category<PS> {
    pub fn id(&self) -> CategoryId {
        CategoryId(NonZeroU32::new(PS::deref(&self.metadata).id.get()).unwrap())
    }

    pub fn name_bytes(&self) -> &[u8] {
        PS::deref_slice(&self.data)
    }
}

impl<PS: ParseStrategy> ValidateArray<CategoryMetadata, u8> for Category<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`CategoryMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a CategoryMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct CategoryMetadata {
    length: le::U32,
    id: le::U32,
    is_alias: le::U32,
}

impl Counted for CategoryMetadata {
    /// [`CategoryMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for CategoryMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`CategoryMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        NonZeroU32::new(self.id.get()).ok_or(ValidateError::NonOptionalIdIsZero)?;
        Ok(())
    }
}
