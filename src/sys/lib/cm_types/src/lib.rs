// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A crate containing common Component Manager types used in Component Manifests
//! (`.cml` files and binary `.cm` files). These types come with `serde` serialization
//! and deserialization implementations that perform the required validation.

use {
    fidl_fuchsia_component_decl as fdecl,
    flyweights::FlyStr,
    lazy_static::lazy_static,
    namespace::{Path as NamespacePath, PathError},
    serde::{de, ser},
    serde::{Deserialize, Serialize},
    std::{
        cmp,
        fmt::{self, Display},
        iter,
        path::PathBuf,
        str::FromStr,
    },
    thiserror::Error,
};

lazy_static! {
    /// A default base URL from which to parse relative component URL
    /// components.
    static ref DEFAULT_BASE_URL: url::Url = url::Url::parse("relative:///").unwrap();
}

/// Generate `impl From` for two trivial enums with identical values, allowing
/// converting to/from each other.
/// This is useful if you have a FIDL-generated enum and a hand-rolled
/// one that contain the same values.
/// # Arguments
///
/// * `$a`, `$b` - The enums to generate `impl From` for. Order doesn't matter because
///     implementation will be generated for both. Enums should be trivial.
/// * `id` - Exhaustive list of all enum values.
/// # Examples
///
/// ```
/// mod a {
///     #[derive(Debug, PartialEq, Eq)]
///     pub enum Streetlight {
///         Green,
///         Yellow,
///         Red,
///     }
/// }
///
/// mod b {
///     #[derive(Debug, PartialEq, Eq)]
///     pub enum Streetlight {
///         Green,
///         Yellow,
///         Red,
///     }
/// }
///
/// symmetrical_enums!(a::Streetlight, b::Streetlight, Green, Yellow, Red);
///
/// assert_eq!(a::Streetlight::Green, b::Streetlight::Green.into());
/// assert_eq!(b::Streetlight::Green, a::Streetlight::Green.into());
/// ```
#[macro_export]
macro_rules! symmetrical_enums {
    ($a:ty , $b:ty, $($id: ident),*) => {
        impl From<$a> for $b {
            fn from(input: $a) -> Self {
                match input {
                    $( <$a>::$id => <$b>::$id, )*
                }
            }
        }

        impl From<$b> for $a {
            fn from(input: $b) -> Self {
                match input {
                    $( <$b>::$id => <$a>::$id, )*
                }
            }
        }
    };
}

/// The error representing a failure to parse a type from string.
#[derive(Serialize, Clone, Deserialize, Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// The string did not match a valid value.
    #[error("invalid value")]
    InvalidValue,
    /// The string did not match a valid absolute or relative component URL
    #[error("invalid URL: {details}")]
    InvalidComponentUrl { details: String },
    /// The string was empty.
    #[error("empty")]
    Empty,
    /// The string was too long.
    #[error("too long")]
    TooLong,
    /// The path segment is invalid.
    #[error("invalid path segment")]
    InvalidSegment,
}

pub const MAX_NAME_LENGTH: usize = name::MAX_NAME_LENGTH;
pub const MAX_LONG_NAME_LENGTH: usize = 1024;
pub const MAX_PATH_LENGTH: usize = namespace::MAX_PATH_LENGTH;
pub const MAX_URL_LENGTH: usize = 4096;

/// A name that can refer to a component, collection, or other entity in the
/// Component Manifest. Its length is bounded to `MAX_NAME_LENGTH`.
pub type Name = BoundedName<MAX_NAME_LENGTH>;
/// A `Name` with a higher string capacity of `MAX_LONG_NAME_LENGTH`.
pub type LongName = BoundedName<MAX_LONG_NAME_LENGTH>;

/// A `BoundedName` is a `Name` that can have a max length of `N` bytes.
#[derive(Serialize, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct BoundedName<const N: usize>(FlyStr);

impl<const N: usize> BoundedName<N> {
    /// Creates a `BoundedName` from a `String`, returning an `Err` if the string
    /// fails validation. The string must be non-empty, no more than `N`
    /// characters in length, and consist of one or more of the
    /// following characters: `A-Z`, `a-z`, `0-9`, `_`, `.`, `-`. It may not start
    /// with `.` or `-`.
    pub fn new(name: impl AsRef<str> + Into<String>) -> Result<Self, ParseError> {
        Self::validate(name.as_ref())?;
        Ok(Self(FlyStr::new(name)))
    }

    pub fn validate(name: &str) -> Result<(), ParseError> {
        if name.is_empty() {
            return Err(ParseError::Empty);
        }
        if name.len() > N {
            return Err(ParseError::TooLong);
        }
        let mut char_iter = name.chars();
        let first_char = char_iter.next().unwrap();
        if !first_char.is_ascii_alphanumeric() && first_char != '_' {
            return Err(ParseError::InvalidValue);
        }
        let valid_fn = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
        if !char_iter.all(valid_fn) {
            return Err(ParseError::InvalidValue);
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<const N: usize> TryFrom<FlyStr> for BoundedName<N> {
    type Error = ParseError;

    fn try_from(flystr: FlyStr) -> Result<Self, Self::Error> {
        Self::validate(flystr.as_str())?;
        Ok(Self(flystr))
    }
}

impl<'a, const N: usize> From<&'a BoundedName<N>> for &'a FlyStr {
    fn from(o: &'a BoundedName<N>) -> Self {
        &o.0
    }
}

impl<const N: usize> PartialEq<&str> for BoundedName<N> {
    fn eq(&self, o: &&str) -> bool {
        &*self.0 == *o
    }
}

impl<const N: usize> PartialEq<String> for BoundedName<N> {
    fn eq(&self, o: &String) -> bool {
        &*self.0 == *o
    }
}

impl<const N: usize> fmt::Display for BoundedName<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        <FlyStr as fmt::Display>::fmt(&self.0, f)
    }
}

impl<const N: usize> FromStr for BoundedName<N> {
    type Err = ParseError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

impl<const N: usize> From<BoundedName<N>> for String {
    fn from(name: BoundedName<N>) -> String {
        name.0.into()
    }
}

impl<'de, const N: usize> de::Deserialize<'de> for BoundedName<N> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor<const N: usize>;

        impl<'de, const N: usize> de::Visitor<'de> for Visitor<N> {
            type Value = BoundedName<{ N }>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&format!(
                    "a non-empty string no more than {} characters in length, \
                    consisting of [A-Za-z0-9_.-] and starting with [A-Za-z0-9_]",
                    N
                ))
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidValue => E::invalid_value(
                        de::Unexpected::Str(s),
                        &"a name that consists of [A-Za-z0-9_.-] and starts with [A-Za-z0-9_]",
                    ),
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &format!("a non-empty name no more than {} characters in length", N)
                            .as_str(),
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

/// A filesystem path.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Path(NamespacePath);

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ser::Serialize for Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl Path {
    /// Creates a [`Path`] from a [`String`], returning an `Err` if the string fails
    /// validation. The string must be non-empty, no more than [`MAX_PATH_LENGTH`] bytes
    /// in length, start with a leading `/`, not be exactly `/`, not have embedded NULs,
    /// and be made up of valid fuchsia.io names. As a result, [`Path`]s are always valid
    /// [`NamespacePath`]s.
    pub fn new(path: impl AsRef<str> + Into<String>) -> Result<Self, ParseError> {
        match NamespacePath::new(path) {
            Ok(path) => {
                if path.as_ref() == "/" {
                    // The `/` is a valid [`NamespacePath`], but the way capabilities
                    // are installed into namespaces require [`Path`]s to always have
                    // a parent, so `/` is an invalid [`Path`].
                    Err(ParseError::InvalidValue)
                } else {
                    Ok(Path(path))
                }
            }
            Err(e) => match e {
                PathError::Empty => Err(ParseError::Empty),
                PathError::TooLong(_) => Err(ParseError::TooLong),
                PathError::InvalidSegment(name::ParseNameError::TooLong(_)) => {
                    Err(ParseError::TooLong)
                }
                PathError::InvalidSegment(_) => Err(ParseError::InvalidSegment),
                PathError::NotUtf8(_) | PathError::NoLeadingSlash(_) => {
                    Err(ParseError::InvalidValue)
                }
            },
        }
    }

    /// Splits the path according to "/".
    pub fn split(&self) -> Vec<String> {
        self.0.split()
    }

    pub fn as_str(&self) -> &str {
        &self.0.as_str()
    }

    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(self.0.to_string())
    }

    pub fn dirname(&self) -> &str {
        self.0.dirname()
    }

    pub fn basename(&self) -> &str {
        self.0.basename()
    }
}

impl IterablePath for Path {
    fn iter_segments(&self) -> Box<dyn DoubleEndedIterator<Item = &str> + '_> {
        Box::new(self.0.iter_segments())
    }
}

impl From<Path> for NamespacePath {
    fn from(value: Path) -> Self {
        value.0
    }
}

impl FromStr for Path {
    type Err = ParseError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Self::new(path)
    }
}

impl AsRef<NamespacePath> for Path {
    fn as_ref(&self) -> &NamespacePath {
        &self.0
    }
}

impl From<Path> for String {
    fn from(path: Path) -> String {
        path.0.to_string()
    }
}

impl<'de> de::Deserialize<'de> for Path {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Path;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH characters \
                     in length, with a leading `/`, and containing no \
                     empty path segments",
                )
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| {
                    match err {
                    ParseError::InvalidValue | ParseError::InvalidSegment => E::invalid_value(
                        de::Unexpected::Str(s),
                        &"a path with leading `/` and non-empty segments, where each segment is no \
                        more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., \
                        and cannot contain embedded NULs",
                    ),
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &"a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH bytes \
                        in length",
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

/// A relative filesystem path.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RelativePath(FlyStr);

impl RelativePath {
    /// Creates a `RelativePath` from a `String`, returning an `Err` if the string fails
    /// validation. The string must be non-empty, no more than [`MAX_PATH_LENGTH`] characters
    /// in length, not start with a `/`, and contain no empty path segments.
    pub fn new(path: impl AsRef<str> + Into<String>) -> Result<Self, ParseError> {
        let p = path.as_ref();
        if p.is_empty() {
            return Err(ParseError::Empty);
        }
        if p.len() > MAX_PATH_LENGTH {
            return Err(ParseError::TooLong);
        }
        for segment in p.split('/') {
            if segment.is_empty() {
                return Err(ParseError::InvalidValue);
            }
            name::validate_name(segment).map_err(|_| ParseError::InvalidSegment)?;
        }
        return Ok(Self(FlyStr::new(path)));
    }

    pub fn as_str(&self) -> &str {
        &*self.0
    }
}

impl IterablePath for RelativePath {
    fn iter_segments(&self) -> Box<dyn DoubleEndedIterator<Item = &str> + '_> {
        let i = self
            .0
            .as_str()
            .split('/')
            // `split('/')` produces empty segments if there is nothing before or after a slash.
            .filter(|s| !s.is_empty());
        Box::new(i)
    }
}

impl FromStr for RelativePath {
    type Err = ParseError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Self::new(path)
    }
}

impl From<RelativePath> for String {
    fn from(path: RelativePath) -> String {
        path.0.into()
    }
}

impl fmt::Debug for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ser::Serialize for RelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> de::Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = RelativePath;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH characters \
                     in length, not starting with `/`, and containing no \
                     empty path segments",
                )
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidValue | ParseError::InvalidSegment => E::invalid_value(
                        de::Unexpected::Str(s),
                        &"a path with no leading `/` and non-empty segments",
                    ),
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &"a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH characters \
                        in length",
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

/// Path that separates the dirname and basename as different variables
/// (referencing type). Convenient for / path representations that split the
/// dirname and basename, like Fuchsia component decl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedSeparatedPath<'a> {
    pub dirname: Option<&'a RelativePath>,
    pub basename: &'a str,
}

impl BorrowedSeparatedPath<'_> {
    /// Converts this [BorrowedSeparatedPath] to the owned type.
    pub fn to_owned(&self) -> SeparatedPath {
        SeparatedPath { dirname: self.dirname.map(Clone::clone), basename: self.basename.into() }
    }
}

impl fmt::Display for BorrowedSeparatedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(dirname) = &self.dirname {
            write!(f, "{}/{}", dirname, self.basename)
        } else {
            write!(f, "{}", self.basename)
        }
    }
}

impl IterablePath for BorrowedSeparatedPath<'_> {
    fn iter_segments(&self) -> Box<dyn DoubleEndedIterator<Item = &str> + '_> {
        if let Some(d) = self.dirname {
            Box::new(d.iter_segments().chain(iter::once(self.basename)))
        } else {
            Box::new(iter::once(self.basename))
        }
    }
}

/// Path that separates the dirname and basename as different variables (owned
/// type). Convenient for path representations that split the dirname and
/// basename, like Fuchsia component decl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparatedPath {
    pub dirname: Option<RelativePath>,
    pub basename: String,
}

impl SeparatedPath {
    /// Obtains a reference to this [SeparatedPath] as the borrowed type.
    pub fn as_ref(&self) -> BorrowedSeparatedPath<'_> {
        BorrowedSeparatedPath { dirname: self.dirname.as_ref(), basename: &self.basename }
    }
}

impl IterablePath for SeparatedPath {
    fn iter_segments(&self) -> Box<dyn DoubleEndedIterator<Item = &str> + '_> {
        if let Some(d) = &self.dirname {
            Box::new(d.iter_segments().chain(iter::once(self.basename.as_str())))
        } else {
            Box::new(iter::once(self.basename.as_str()))
        }
    }
}

impl fmt::Display for SeparatedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(dirname) = &self.dirname {
            write!(f, "{}/{}", dirname, self.basename)
        } else {
            write!(f, "{}", self.basename)
        }
    }
}

/// Trait implemented by path types that provides an API to iterate over path segments.
pub trait IterablePath: Clone + Send + Sync {
    /// Returns a double-sided iterator over the segments in this path.
    fn iter_segments(&self) -> Box<dyn DoubleEndedIterator<Item = &str> + '_>;
}

/// A component URL. The URL is validated, but represented as a string to avoid
/// normalization and retain the original representation.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Url(FlyStr);

impl Url {
    /// Creates a `Url` from a `String`, returning an `Err` if the string fails
    /// validation. The string must be non-empty, no more than 4096 characters
    /// in length, and be a valid URL. See the [`url`](../../url/index.html) crate.
    pub fn new(url: impl AsRef<str> + Into<String>) -> Result<Self, ParseError> {
        Self::validate(url.as_ref())?;
        Ok(Self(FlyStr::new(url)))
    }

    /// Verifies the given string is a valid absolute or relative component URL.
    pub fn validate(url_str: &str) -> Result<(), ParseError> {
        if url_str.is_empty() {
            return Err(ParseError::Empty);
        }
        if url_str.len() > MAX_URL_LENGTH {
            return Err(ParseError::TooLong);
        }
        match url::Url::parse(url_str).map(|url| (url, false)).or_else(|err| {
            if err == url::ParseError::RelativeUrlWithoutBase {
                DEFAULT_BASE_URL.join(url_str).map(|url| (url, true))
            } else {
                Err(err)
            }
        }) {
            Ok((url, is_relative)) => {
                let mut path = url.path();
                if path.starts_with('/') {
                    path = &path[1..];
                }
                if is_relative && url.fragment().is_none() {
                    // TODO(https://fxbug.dev/42070831): Fragments should be optional
                    // for relative path URLs.
                    //
                    // Historically, a component URL string without a scheme
                    // was considered invalid, unless it was only a fragment.
                    // Subpackages allow a relative path URL, and by current
                    // definition they require a fragment. By declaring a
                    // relative path without a fragment "invalid", we can avoid
                    // breaking tests that expect a path-only string to be
                    // invalid. Sadly this appears to be a behavior of the
                    // public API.
                    return Err(ParseError::InvalidComponentUrl {
                        details: "Relative URL has no resource fragment.".to_string(),
                    });
                }
                if url.host_str().unwrap_or("").is_empty()
                    && path.is_empty()
                    && url.fragment().is_none()
                {
                    return Err(ParseError::InvalidComponentUrl {
                        details: "URL is missing either `host`, `path`, and/or `resource`."
                            .to_string(),
                    });
                }
            }
            Err(err) => {
                return Err(ParseError::InvalidComponentUrl {
                    details: format!("Malformed URL: {err:?}."),
                });
            }
        }
        // Use the unparsed URL string so that the original format is preserved.
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &*self.0
    }
}

impl FromStr for Url {
    type Err = ParseError;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        Self::new(url)
    }
}

impl From<Url> for String {
    fn from(url: Url) -> String {
        (*url.0).to_owned()
    }
}

impl<'de> de::Deserialize<'de> for Url {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Url;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a non-empty URL no more than 4096 characters in length")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidComponentUrl { details: _ } => {
                        E::invalid_value(de::Unexpected::Str(s), &"a valid URL")
                    }
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &"a non-empty URL no more than 4096 characters in length",
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

/// A URL scheme.
#[derive(Serialize, Clone, Debug, Eq, Hash, PartialEq)]
pub struct UrlScheme(FlyStr);

impl UrlScheme {
    /// Creates a `UrlScheme` from a `String`, returning an `Err` if the string fails
    /// validation. The string must be non-empty and no more than 100 characters
    /// in length. It must start with a lowercase ASCII letter (a-z),
    /// and contain only lowercase ASCII letters, digits, `+`, `-`, and `.`.
    pub fn new(url_scheme: impl AsRef<str> + Into<String>) -> Result<Self, ParseError> {
        Self::validate(url_scheme.as_ref())?;
        Ok(UrlScheme(FlyStr::new(url_scheme)))
    }

    /// Validates `url_scheme` but does not construct a new `UrlScheme` object.
    /// See [`UrlScheme::new`] for validation details.
    pub fn validate(url_scheme: &str) -> Result<(), ParseError> {
        if url_scheme.is_empty() {
            return Err(ParseError::Empty);
        }
        if url_scheme.len() > MAX_NAME_LENGTH {
            return Err(ParseError::TooLong);
        }
        let mut iter = url_scheme.chars();
        let first_char = iter.next().unwrap();
        if !first_char.is_ascii_lowercase() {
            return Err(ParseError::InvalidValue);
        }
        if let Some(_) = iter.find(|&c| {
            !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '.' && c != '+' && c != '-'
        }) {
            return Err(ParseError::InvalidValue);
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &*self.0
    }
}

impl fmt::Display for UrlScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for UrlScheme {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl From<UrlScheme> for String {
    fn from(u: UrlScheme) -> String {
        u.0.into()
    }
}

impl<'de> de::Deserialize<'de> for UrlScheme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = UrlScheme;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a non-empty URL scheme no more than 100 characters in length")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidValue => {
                        E::invalid_value(de::Unexpected::Str(s), &"a valid URL scheme")
                    }
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &"a non-empty URL scheme no more than 100 characters in length",
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

/// The duration of child components in a collection. See [`Durability`].
///
/// [`Durability`]: ../../fidl_fuchsia_sys2/enum.Durability.html
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    Transient,
    /// An instance is started on creation and exists until it stops.
    SingleRun,
}

symmetrical_enums!(Durability, fdecl::Durability, Transient, SingleRun);

/// A component instance's startup mode. See [`StartupMode`].
///
/// [`StartupMode`]: ../../fidl_fuchsia_sys2/enum.StartupMode.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StartupMode {
    Lazy,
    Eager,
}

impl StartupMode {
    pub fn is_lazy(&self) -> bool {
        matches!(self, StartupMode::Lazy)
    }
}

symmetrical_enums!(StartupMode, fdecl::StartupMode, Lazy, Eager);

impl Default for StartupMode {
    fn default() -> Self {
        Self::Lazy
    }
}

/// A component instance's recovery policy. See [`OnTerminate`].
///
/// [`OnTerminate`]: ../../fidl_fuchsia_sys2/enum.OnTerminate.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OnTerminate {
    None,
    Reboot,
}

symmetrical_enums!(OnTerminate, fdecl::OnTerminate, None, Reboot);

impl Default for OnTerminate {
    fn default() -> Self {
        Self::None
    }
}

/// The kinds of offers that can target components in a given collection. See
/// [`AllowedOffers`].
///
/// [`AllowedOffers`]: ../../fidl_fuchsia_sys2/enum.AllowedOffers.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AllowedOffers {
    StaticOnly,
    StaticAndDynamic,
}

symmetrical_enums!(AllowedOffers, fdecl::AllowedOffers, StaticOnly, StaticAndDynamic);

impl Default for AllowedOffers {
    fn default() -> Self {
        Self::StaticOnly
    }
}

/// Offered dependency type. See [`DependencyType`].
///
/// [`DependencyType`]: ../../fidl_fuchsia_sys2/enum.DependencyType.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyType {
    Strong,
    Weak,
    WeakForMigration,
}

symmetrical_enums!(DependencyType, fdecl::DependencyType, Strong, Weak, WeakForMigration);

impl Default for DependencyType {
    fn default() -> Self {
        Self::Strong
    }
}

/// Capability availability. See [`Availability`].
///
/// [`Availability`]: ../../fidl_fuchsia_sys2/enum.Availability.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Required,
    Optional,
    SameAsTarget,
    Transitional,
}

symmetrical_enums!(
    Availability,
    fdecl::Availability,
    Required,
    Optional,
    SameAsTarget,
    Transitional
);

impl Display for Availability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Availability::Required => write!(f, "Required"),
            Availability::Optional => write!(f, "Optional"),
            Availability::SameAsTarget => write!(f, "SameAsTarget"),
            Availability::Transitional => write!(f, "Transitional"),
        }
    }
}

// TODO(cgonyeo): remove this once we've soft migrated to the availability field being required.
impl Default for Availability {
    fn default() -> Self {
        Self::Required
    }
}

impl PartialOrd for Availability {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        match (*self, *other) {
            (Availability::Transitional, Availability::Optional)
            | (Availability::Transitional, Availability::Required)
            | (Availability::Optional, Availability::Required) => Some(cmp::Ordering::Less),
            (Availability::Optional, Availability::Transitional)
            | (Availability::Required, Availability::Transitional)
            | (Availability::Required, Availability::Optional) => Some(cmp::Ordering::Greater),
            (Availability::Required, Availability::Required)
            | (Availability::Optional, Availability::Optional)
            | (Availability::Transitional, Availability::Transitional)
            | (Availability::SameAsTarget, Availability::SameAsTarget) => {
                Some(cmp::Ordering::Equal)
            }
            (Availability::SameAsTarget, _) | (_, Availability::SameAsTarget) => None,
        }
    }
}

/// Specifies when the framework will open the protocol from the provider
/// component's outgoing directory when someone requests the capability. See
/// [`DeliveryType`].
///
/// [`DeliveryType`]: ../../fidl_fuchsia_component_decl/enum.DeliveryType.html
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryType {
    Immediate,
    OnReadable,
}

impl TryFrom<fdecl::DeliveryType> for DeliveryType {
    type Error = fdecl::DeliveryType;

    fn try_from(value: fdecl::DeliveryType) -> Result<Self, Self::Error> {
        match value {
            fdecl::DeliveryType::Immediate => Ok(DeliveryType::Immediate),
            fdecl::DeliveryType::OnReadable => Ok(DeliveryType::OnReadable),
            fdecl::DeliveryTypeUnknown!() => Err(value),
        }
    }
}

impl From<DeliveryType> for fdecl::DeliveryType {
    fn from(value: DeliveryType) -> Self {
        match value {
            DeliveryType::Immediate => fdecl::DeliveryType::Immediate,
            DeliveryType::OnReadable => fdecl::DeliveryType::OnReadable,
        }
    }
}

impl Display for DeliveryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryType::Immediate => write!(f, "Immediate"),
            DeliveryType::OnReadable => write!(f, "OnReadable"),
        }
    }
}

impl Default for DeliveryType {
    fn default() -> Self {
        Self::Immediate
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageId {
    StaticInstanceId,
    StaticInstanceIdOrMoniker,
}

symmetrical_enums!(StorageId, fdecl::StorageId, StaticInstanceId, StaticInstanceIdOrMoniker);

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json, std::iter::repeat};

    macro_rules! expect_ok {
        ($type_:ty, $($input:tt)+) => {
            assert!(serde_json::from_str::<$type_>(&json!($($input)*).to_string()).is_ok());
        };
    }

    macro_rules! expect_err {
        ($type_:ty, $($input:tt)+) => {
            assert!(serde_json::from_str::<$type_>(&json!($($input)*).to_string()).is_err());
        };
    }

    #[test]
    fn test_valid_name() {
        expect_ok!(Name, "foo");
        expect_ok!(Name, "Foo");
        expect_ok!(Name, "O123._-");
        expect_ok!(Name, "_O123._-");
        expect_ok!(Name, repeat("x").take(255).collect::<String>());
    }

    #[test]
    fn test_invalid_name() {
        expect_err!(Name, "");
        expect_err!(Name, "-");
        expect_err!(Name, ".");
        expect_err!(Name, "@&%^");
        expect_err!(Name, repeat("x").take(256).collect::<String>());
    }

    #[test]
    fn test_valid_path() {
        expect_ok!(Path, "/foo");
        expect_ok!(Path, "/foo/bar");
        expect_ok!(Path, &format!("/{}", repeat("x").take(255).collect::<String>()));
        // 2047 * 2 characters per repeat = 4094
        expect_ok!(Path, &repeat("/x").take(2047).collect::<String>());
    }

    #[test]
    fn test_invalid_path() {
        expect_err!(Path, "");
        expect_err!(Path, "/");
        expect_err!(Path, "foo");
        expect_err!(Path, "foo/");
        expect_err!(Path, "/foo/");
        expect_err!(Path, "/foo//bar");
        expect_err!(Path, "/fo\0b/bar");
        expect_err!(Path, &format!("/{}", repeat("x").take(256).collect::<String>()));
        // 2048 * 2 characters per repeat = 4096
        expect_err!(Path, &repeat("/x").take(2048).collect::<String>());
    }

    #[test]
    fn test_path_dirname_basename() {
        let path = Path::new("/foo").unwrap();
        assert_eq!((path.dirname(), path.basename()), ("/", "foo"));
        let path = Path::new("/foo/bar").unwrap();
        assert_eq!((path.dirname(), path.basename()), ("/foo", "bar"));
        let path = Path::new("/foo/bar/baz").unwrap();
        assert_eq!((path.dirname(), path.basename()), ("/foo/bar", "baz"));
    }

    #[test]
    fn test_separated_path() {
        fn test_path(path: SeparatedPath, expected_segments: Vec<&str>) {
            let segments: Vec<_> = path.iter_segments().collect();
            assert_eq!(segments, expected_segments);
            let borrowed_path = path.as_ref();
            let segments: Vec<_> = borrowed_path.iter_segments().collect();
            assert_eq!(segments, expected_segments);
            let owned_path = borrowed_path.to_owned();
            assert_eq!(path, owned_path);
            let expected_fmt = expected_segments.join("/");
            assert_eq!(format!("{path}"), expected_fmt);
            assert_eq!(format!("{owned_path}"), expected_fmt);
        }
        test_path(SeparatedPath { dirname: None, basename: "foo".into() }, vec!["foo"]);
        test_path(
            SeparatedPath { dirname: Some("bar".parse().unwrap()), basename: "foo".into() },
            vec!["bar", "foo"],
        );
        test_path(
            SeparatedPath { dirname: Some("bar/baz".parse().unwrap()), basename: "foo".into() },
            vec!["bar", "baz", "foo"],
        );
    }

    #[test]
    fn test_valid_relative_path() {
        expect_ok!(RelativePath, "foo");
        expect_ok!(RelativePath, "foo/bar");
        expect_ok!(RelativePath, &format!("x{}", repeat("/x").take(2047).collect::<String>()));
    }

    #[test]
    fn test_invalid_relative_path() {
        expect_err!(RelativePath, "");
        expect_err!(RelativePath, "/");
        expect_err!(RelativePath, "/foo");
        expect_err!(RelativePath, "foo/");
        expect_err!(RelativePath, "/foo/");
        expect_err!(RelativePath, "foo//bar");
        expect_err!(RelativePath, "foo/..");
        expect_err!(RelativePath, &format!("x{}", repeat("/x").take(2048).collect::<String>()));
    }

    #[test]
    fn test_valid_url() {
        expect_ok!(Url, "a://foo");
        expect_ok!(Url, "#relative-url");
        expect_ok!(Url, &format!("a://{}", repeat("x").take(4092).collect::<String>()));
    }

    #[test]
    fn test_invalid_url() {
        expect_err!(Url, "");
        expect_err!(Url, "foo");
        expect_err!(Url, &format!("a://{}", repeat("x").take(4093).collect::<String>()));
    }

    #[test]
    fn test_valid_url_scheme() {
        expect_ok!(UrlScheme, "fuch.sia-pkg+0");
        expect_ok!(UrlScheme, &format!("{}", repeat("f").take(255).collect::<String>()));
    }

    #[test]
    fn test_invalid_url_scheme() {
        expect_err!(UrlScheme, "");
        expect_err!(UrlScheme, "0fuch.sia-pkg+0");
        expect_err!(UrlScheme, "fuchsia_pkg");
        expect_err!(UrlScheme, "FUCHSIA-PKG");
        expect_err!(UrlScheme, &format!("{}", repeat("f").take(256).collect::<String>()));
    }

    #[test]
    fn test_name_error_message() {
        let input = r#"
            "foo$"
        "#;
        let err = serde_json::from_str::<Name>(input).expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "invalid value: string \"foo$\", expected a name \
            that consists of [A-Za-z0-9_.-] and starts with [A-Za-z0-9_] \
            at line 2 column 18"
        );
        assert_eq!(err.line(), 2);
        assert_eq!(err.column(), 18);
    }

    #[test]
    fn test_path_error_message() {
        let input = r#"
            "foo";
        "#;
        let err = serde_json::from_str::<Path>(input).expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "invalid value: string \"foo\", expected a path with leading `/` and non-empty \
            segments, where each segment is no \
            more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., \
            and cannot contain embedded NULs at line 2 column 17"
        );

        assert_eq!(err.line(), 2);
        assert_eq!(err.column(), 17);
    }

    #[test]
    fn test_url_error_message() {
        let input = r#"
            "foo";
        "#;
        let err = serde_json::from_str::<Url>(input).expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "invalid value: string \"foo\", expected a valid URL at line 2 \
             column 17"
        );
        assert_eq!(err.line(), 2);
        assert_eq!(err.column(), 17);
    }

    #[test]
    fn test_url_scheme_error_message() {
        let input = r#"
            "9fuchsia_pkg"
        "#;
        let err = serde_json::from_str::<UrlScheme>(input).expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "invalid value: string \"9fuchsia_pkg\", expected a valid URL scheme at line 2 column 26"
        );
        assert_eq!(err.line(), 2);
        assert_eq!(err.column(), 26);
    }

    #[test]
    fn test_symmetrical_enums() {
        mod a {
            #[derive(Debug, PartialEq, Eq)]
            pub enum Streetlight {
                Green,
                Yellow,
                Red,
            }
        }

        mod b {
            #[derive(Debug, PartialEq, Eq)]
            pub enum Streetlight {
                Green,
                Yellow,
                Red,
            }
        }

        symmetrical_enums!(a::Streetlight, b::Streetlight, Green, Yellow, Red);

        assert_eq!(a::Streetlight::Green, b::Streetlight::Green.into());
        assert_eq!(a::Streetlight::Yellow, b::Streetlight::Yellow.into());
        assert_eq!(a::Streetlight::Red, b::Streetlight::Red.into());
        assert_eq!(b::Streetlight::Green, a::Streetlight::Green.into());
        assert_eq!(b::Streetlight::Yellow, a::Streetlight::Yellow.into());
        assert_eq!(b::Streetlight::Red, a::Streetlight::Red.into());
    }
}
