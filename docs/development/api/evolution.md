# Fuchsia API evolution guidelines

This section contains guidelines for Fuchsia contributors making changes to
Fuchsia Platform APIs. Before you begin, you should be familiarized with the
following concepts:

- [FIDL versioning](/docs/reference/fidl/language/versioning.md)

## The lifecycle of a platform API {#lifecycle}

Fuchsia platform APIs should follow the lifecycle:
_Added → Deprecated → Removed → Deleted_, as illustrated below:

![This image shows the lifecycle of an API on the Fuchsia platform starting
  with the version when the API was added, then deprecated, then removed, and
  finally deprecated](images/platform-api-lifecycle.png "Fuchsia platform API
  lifecycle")

The following sections explain how to manage this lifecycle as an API developer.

### Adding FIDL APIs {#adding}

Always annotate new FIDL APIs with an
[@available](/docs/reference/fidl/language/versioning.md)
attribute. Unstable APIs should be added at the `HEAD`
API level. Note that partners using the SDK cannot target
the `HEAD` API level, by design.

For example:

```fidl
@available(added=HEAD)
library fuchsia.examples.docs;
```

Stable APIs should be `added` at the
[in-development API level](/sdk/version_history.json).
This means that starting at the current in-development API
level, this API is available and will not change without
the appropriate deprecation flows.
For example:

```fidl
// At the time of writing the in development level was 10.
@available(added=10)
library fuchsia.examples.docs;
```

When a FIDL library has more than one `.fidl` file, the library should include a
separate `overview.fidl` file and the `@available` attribute should be written
in that file along with a documentation comment describing the library. See
[the FIDL style guide][fidl-library-style]
for more information.

Every API in the partner [SDK category](/docs/contribute/sdk/categories.md)
is opted into static compatibility testing in CI/CQ. These tests fail when
an API changes in backward incompatible ways. If your API is unstable, consider
adding it to the internal or experimental SDK categories to prevent partners from
depending on it and to opt out of static compatibility tests, allowing the API
to change freely. Once the API is stable, add it to the partner category.

### Replacing FIDL APIs {#replacing}

Sometimes you need to replace an API with a new definition. To do this at API
level `N`, annotate the old definition with `@available(replaced=N)` and the new
definition with `@available(added=N)`. For example, this is how you would change
the value of a constant at API level 5:

```fidl
@available(replaced=5)
const MAX_LENGTH uint32 = 16;
@available(added=5)
const MAX_LENGTH uint32 = 32;
```

### Deprecating FIDL APIs {#deprecating}

You should always deprecate an API at an earlier level than you remove it. When
an end developer targets a deprecated API, they see a warning at build time that
the API is deprecated and they should migrate to an alternative. You should
include a note to help the end developer find an alternative. For example:

```fidl
protocol Example {
    // (Description of the method.)
    //
    // # Deprecation
    //
    // (Detailed explanation of why the method is deprecated, the timeline for
    // removing it, and what should be used instead.)
    @available(deprecated=5, removed=6, note="use Replacement")
    Deprecated();

    @available(added=5)
    Replacement();
};
```

There must be at least one API level between an API's deprecation and removal.
It is perfectly fine, however, to deprecate an API at the same level that it was
added. For example:

```fidl
// These are OK.
@available(deprecated=5, removed=6)
@available(deprecated=5, removed=100)
@available(added=5, deprecated=5)

// These will not compile.
@available(deprecated=5, removed=5)
@available(deprecated=5, removed=3)
```

### Removing FIDL APIs {#removing}

Note that you should always [deprecate](#deprecating) an API
before removing it, and you should [preserve the ABI](#preserving)
when removing an API whenever possible.

The recommended way to remove an API is to use its @available attribute. This is
the method we generally recommend. For example, if an API was added at level 10,
it can be removed at level 12 like this:

```
@available(added=10, removed=12)
library fuchsia.examples.docs;
```

In this example, an end developer targeting levels 10 or 11 would see client
bindings for the fuchsia.examples.docs library, but a developer targeting level
12 or greater would not. If this API's source is removed before the platform
drops support for API level 12, the API's static compatibility tests will fail
and special approval from //sdk/history/OWNERS will be required to submit the
change. When the Fuchsia platform drops support for API level 12 the API's source
code can be deleted.

Alternatively, you can delete the API's source code which is not recommended for
most use cases. If the API was added at the in development API level or a
previous API level which is currently supported,  this removes the API from
Fuchsia's history which is generally not allowed. Static compatibility tests
will fail in this case and you will need special approval from
`//sdk/history/OWNERS` to submit the changes. If the API was added at any level
greater than the in development API level - including the special `HEAD` API
level  - then this method of removal is fine.

### Preserving ABI when removing FIDL APIs {#preserving}

It's possible to remove an API's client bindings from SDKs - preventing future
end developers from targeting the API - while preserving the platform's
implementation of the API (The ABI). This feature allows existing applications
to run on newer versions of the platform. When an API has been removed from SDKs
and the platform still supports its ABI, we say the platform has legacy
[support](/docs/reference/fidl/language/versioning.md#legacy)
for that API.

To maintain legacy support for an API, set legacy=true when removing the API.
For example:

```fidl
protocol LegacyExample {
    @available(added=10, deprecated=11, removed=12, legacy=true)
    LegacyMethod();
};
```

All methods in the Fuchsia platform should retain legacy support when they are
removed. Once the Fuchsia platform drops support for all API levels before the
method's removal, it is safe to remove `legacy=true` and the method's
implementation.

## Designing APIs that evolve gracefully {#evolve-gracefully}

This rubric focuses on promoting compatibility with a range of platform
versions. These attributes make compatibility as easy as possible to maintain
and is a subset of the [FIDL API Rubric][fidl-rubric].

### Follow the FIDL Style Guide

The [FIDL style guidelines][fidl-style] are used to make FIDL readable and
embody best practices. These are generally best practices, and should be
followed regardless of sdk_category.

### Use FIDL Versioning annotations

The [FIDL versioning annotations][fidl-versioning] allow libraries, protocols,
and other elements to be associated with specific API levels. All compatibility
reasoning is based on API version. This is how to express a point in the
evolution of an API.

- Only ever modify an API at the `in-development` or `HEAD` API level.

- Once an API level is declared stable, it should not be changed. (see
  [version_history.json][version-json]). This allows changes to the API, while
  existing API levels are unchanged.

### Specify bounds for vector and string

More information:
[FIDL API Rubric - Specify bounds for vector and string][fidl-string]

### Use enum vs. boolean

Since booleans are binary, the use of enum which can have multiple states is
preferred when making APIs compatibility-friendly. This way if an additional
state is needed, the enum can be extended, whereas the boolean would have to be
replaced with another type. More information:
[FIDL API Rubric - Avoid booleans if more states are possible][fidl-bool].

### Use flexible enums and bits

Flexible enums have a default unknown member, so it allows for easy evolution
of the enum.

Only use `strict` `enum` and `bits` types when you are _extremely_ confident
they will never be extended. `strict` `enum` and `bits` types cannot be
extended, and migrating them to `flexible` requires a migration for every field
with the given type.

More information: [FIDL Language - Strict vs. Flexible][fidl-enum]

### Prefer tables over structs

Both structs and tables represent an object with multiple named fields. The
difference is that structs have a fixed layout in the wire format, which means
they _cannot_ be modified without breaking binary compatibility. By contrast,
tables have a flexible layout in the wire format, which means fields _can_ be
added to a table over time without breaking binary compatibility.

More information:
[FIDL API Rubric - Should I use a struct or table?][fidl-table]

### Use open protocols with flexible methods and events

In general, all protocols should be `open`, and all methods and events within
those protocols should be `flexible`.

Marking a protocol as open makes it easier to deal with removing methods or
events when different components might have been built at different versions,
such that each component has a different view of which methods and events exist.
Because flexibility for evolving protocols is generally desirable, it is
recommended to choose open for protocols unless there is a reason to choose a
more closed protocol.

One potential exception is for _tear-off protocols_, representing a transaction,
where the only two-way method is a commit operation which must be strict while
other operations on the transaction may evolve.. If a protocol is very small,
unlikely to change, and expected to be implemented by clients, you can make it
`closed` and all the methods `strict`. This will spare the client the trouble of
deciding how to handle an "unknown interaction." The cost, however, is that
methods or events can never be added to or removed from such a protocol. If you
decide you _do_ want to add a method or event, you'll need to define a new
tear-off protocol to replace it.

More information:

- [FIDL API Rubric - strict or flexible?][fidl-flexible]
- [FIDL API Rubric - Open, ajar, or closed?][fidl-open]

### Use the error syntax

The [error syntax][fidl-error-syntax] is used to specify a method will return a
value, or error out and return an int or enum representing the error.

## Use a custom error enum, not zx.Status

Use a purpose built enum error type when you define and control the domain. For
example, define an enum when the protocol is purpose built, and conveying the
semantics of the error is the only design constraint.

Use a domain-specific enum error type when you are following a well defined
specification (say HTTP error codes), and the enum is meant to be an ergonomic
way to represent the raw value dictated by the specification.

More information:
[FIDL API Rubric - Prefer domain specific enum for errors][fidl-error-enum].

### Don't use declarations from other libraries

It's good for a public API to reuse types and compose protocols if they're
semantically equivalent, but it's easy to make mistakes.

[fidl-bool]: /docs/development/api/fidl.md#avoid_booleans_if_more_states_are_possible
[fidl-enum]: /docs/reference/fidl/language/language.md#strict-vs-flexible
[fidl-error-enum]: /docs/development/api/fidl.md#prefer-domain-specific-enum-for-errors
[fidl-error-syntax]: /docs/development/api/fidl.md#error-syntax
[fidl-flexible]: /docs/development/api/fidl.md#strict-flexible-method
[fidl-open]: /docs/development/api/fidl.md#open-ajar-closed
[fidl-table]: /docs/development/api/fidl.md#should-i-use-a-struct-or-a-table
[fidl-rubric]: /docs/development/api/fidl.md
[fidl-style]: /docs/development/languages/fidl/guides/style.md
[fidl-library-style]: /docs/development/languages/fidl/guides/style.md#library-overview
[fidl-string]: /docs/development/api/fidl.md#specify_bounds_for_vector_and_string
[fidl-versioning]: /docs/reference/fidl/language/versioning.md
[version-json]: https://cs.opensource.google/fuchsia/fuchsia/+/main:/sdk/version_history.json
