load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_bind_cc_library",
    "fuchsia_bind_library",
)

fuchsia_bind_library(
    name = "{{name}}",
    deps = [
        {{deps}}
    ],
    srcs = [
        {{sources}}
    ],
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)

fuchsia_bind_cc_library(
    name = "{{name}}_cc",
    library = "{{name}}",
    deps = [
        {{cc_deps}}
    ],
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
