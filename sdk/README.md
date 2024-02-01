# Fuchsia IDK

This directory contains build instructions for the core of Fuchsia, the
[Fuchsia Integrator Development Kit (IDK)](../docs/glossary.md#fuchsia-idk).
The IDK is produced (or built) by processing the contents of this directory.

Software outside of the [Platform Source
Tree](../docs/glossary.md#platform-source-tree) should depend only on the Fuchsia
IDK.

> [Learn more](../docs/development/sdk/)

Developer-facing development kits are then derived from the IDK. For example
(this list is not exhaustive):

- Software Development Kit (SDK)
- Product Development Kit (PDK)

## Categories

Not all the interfaces defined in this directory are part of every Fuchsia SDK.
Instead, interfaces have a `category` label that determines whether the
interface can be included in a given SDK. For example, interfaces with the
`internal` category are available only within the
[Platform Source Tree](../docs/glossary.md#platform-source-tree).
Interfaces with the `partner` category are additionally available to partner
projects. See [sdk_atom.gni](../build/sdk/sdk_atom.gni) for more details.

## Version history

The `version_history.json` file is not yet fully baked. Please use with
caution.

## Governance

The API surface described by the IDK is governed by the [Fuchsia API
Council](/docs/contribute/governance/api_council.md) and should conform to the
appropriate [API rubrics](/docs/development/api/README.md).

## Listing of FIDL libraries

Within the Fuchsia build, all FIDL libraries which are included in the SDK
are listed in the `sdk_fidl_json.json` file.

See [`//build/fidl:sdk_fidl`](/build/fidl/BUILD.gn) and
[fidl.gni](/build/fidl/fidl.gni) for further details.
