# Fuchsia SDK

This section is for developing Fuchsia software using the Fuchsia SDK.

Important: The Fuchsia SDK is in active development. At the moment, Fuchsia
does not support general public usage of the Fuchsia SDK. The APIs in the SDK
are subject to change without notice.

## Developing software with the Fuchsia SDK

The Fuchsia SDK is a set of build rules, API headers, prebuilt and source code
libraries, and [host tools][host-tools] put together to enable a Fuchsia software
development environment. With the Fuchsia SDK, developers can create, build, run,
test, and debug [Fuchsia components][fuchsia-components] and drivers (that is,
Fuchsia software) without needing to set up a
[Fuchsia source checkout][fuchsia-platform] (`fuchsia.git`) on the host machine.

Note: If you only need the latest copies of Fuchsia's host tools
(for instance, `ffx`), you can download the Fuchsia IDK from the
[Download the Fuchsia IDK][download-idk] page to get the up-to-date builds
of Fuchsia's host tools.

## Build system support

The Fuchsia SDK supports [Bazel][bazel]{:.external} as  an
out-of-the-box solution for building and testing software. However, Bazel is not
a strict requirement. The Fuchsia SDK is designed to be integrated with
most build systems to meet the needs of a diverse development ecosystem.

## Workflows using the Fuchsia SDK

If you're new to the Fuchsia SDK, complete the
[Walkthrough of workflows using the Fuchsia SDK][get-started-sdk] guide to learn
about the basic workflows in Fuchsia component development using the Fuchsia SDK.

## Driver development workflows using the Fuchsia SDK

If you're interested in developing a new driver for Fuchsia, complete the
[Walkthrough of driver development workflows using the Fuchsia SDK][get-started-drivers]
guide to learn about the basic workflows of driver development using the Fuchsia SDK.

<!-- Reference links -->

[host-tools]: https://fuchsia.dev/reference/tools/sdk/ffx
[fuchsia-components]: /docs/concepts/components/v2
[fuchsia-platform]: /docs/development
[bazel]: https://bazel.build/docs
[get-started-sdk]: /docs/get-started/sdk/index.md
[get-started-drivers]: /docs/get-started/sdk/get-started-with-driver.md
[download-idk]: /docs/development/idk/download.md
