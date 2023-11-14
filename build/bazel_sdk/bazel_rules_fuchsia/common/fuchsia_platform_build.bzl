"""Misc utilities that are related to the Fuchsia platform build only."""

load(
    "//:repository_utils.bzl",
    "get_fuchsia_host_arch",
    "get_fuchsia_host_os",
)
load(
    "//:toolchains/clang/clang_utils.bzl",
    "to_clang_target_tuple",
)

def _get_fuchsia_source_relative_path(repo_ctx, path):
    """Return the path of a given file relative to the fuchsia source directory.

    Args:
       repo_ctx: repository context
       path: A path, relative to the fuchsia source directory.

    Returns:
       A new repo_ctx.path() object pointing to the file.
       If repo_ctx.attr.fuchsia_source_dir is empty, this assumes that this
       is run from the Fuchsia platform build, and will record a dependency
       to the resulting file path, to ensure the repository rule is always
       re-run when the file changes.
    """
    if repo_ctx.attr.fuchsia_source_dir:
        return repo_ctx.path("%s/%s/%s" % (
            repo_ctx.workspace_root,
            repo_ctx.attr.fuchsia_source_dir,
            path,
        ))
    else:
        return repo_ctx.path(Label("@//:" + path))

def _get_ninja_output_dir(repo_ctx):
    """Compute the Ninja output directory used by this workspace.

    Args:
        repo_ctx: the repository context.
    Returns:
        a string containing the path to the ninja output directory,
        relative to the current workspace root.
    """

    # The config file at this location contains the location of the TOPDIR
    # used by update-workspace.py, e.g. `gen/build/bazel`. This code assumes
    # that the workspace is one of its sub-directories, so compute a
    # back-tracking path prefix by counting the number of path num_fragment
    # in it.
    config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/bazel/config/main_workspace_top_dir",
    )
    top_dir = repo_ctx.read(config_path).strip()
    num_fragments = len(top_dir.split("/"))
    result = ".."
    for n in range(num_fragments):
        result += "/.."

    if repo_ctx.attr.fuchsia_source_dir:
        result = "%s/%s" % (repo_ctx.attr.fuchsia_source_dir, result)

    # If a build.ninja file does not exist in the resulting file, something
    # is wrong, so return an invalid value that will clarify the issue in
    # future error messages if something tries to use it.
    # Do not fail() because the resulting `build_config` can be used OOT,
    # for example when invoking Bazel directly from //build/bazel_sdk/tests/
    if not repo_ctx.path(result).exists:
        result = "COULD_NOT_FIND_NINJA_OUTPUT_DIRECTORY"

    return result

def _get_rbe_config(repo_ctx):
    """Compute RBE-related configuration.

    Args:
      repo_ctx: Repository context.
    Returns:
      A struct with fields describing RBE-related config information.
    """
    rewrapper_config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/rbe/fuchsia-rewrapper.cfg",
    )

    reproxy_config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/rbe/fuchsia-reproxy.cfg",
    )

    instance_prefix = "instance="

    # Note: platform value is a comma-separated list of key=values.
    # This extraction assumes that "container-image" is the only key-value.
    # If ever there are multiple key-values, this extraction will require
    # a little more parsing to be more robust.
    container_image_prefix = "platform=container-image="

    instance_name = None
    container_image = None

    for line in repo_ctx.read(rewrapper_config_path).splitlines():
        line = line.strip()
        if line.startswith(container_image_prefix):
            container_image = line[len(container_image_prefix):]

    for line in repo_ctx.read(reproxy_config_path).splitlines():
        line = line.strip()
        if line.startswith(instance_prefix):
            instance_name = line[len(instance_prefix):]

    if not instance_name:
        fail("ERROR: Missing instance name from %s" % reproxy_config_path)

    if not container_image:
        fail("ERROR: Missing container image name from %s" % rewrapper_config_path)

    return struct(
        instance_name = instance_name,
        container_image = container_image,
    )

def _fuchsia_build_config_repository_impl(repo_ctx):
    host_os = get_fuchsia_host_os(repo_ctx)
    host_arch = get_fuchsia_host_arch(repo_ctx)
    host_target_triple = to_clang_target_tuple(host_os, host_arch)
    host_tag = "%s-%s" % (host_os, host_arch)
    host_tag_alt = "%s_%s" % (host_os, host_arch)

    host_os_constraint = "@platforms//os:" + {
        "mac": "macos",
    }.get(host_os, host_os)

    host_cpu_constraint = "@platforms//cpu:" + {
        "x64": "x86_64",
        "arm64": "aarch64",
    }.get(host_arch, host_arch)

    rbe_config = _get_rbe_config(repo_ctx)

    ninja_output_dir = _get_ninja_output_dir(repo_ctx)

    defs_content = '''# Auto-generated DO NOT EDIT

build_config = struct(
    # The host operating system, using Fuchsia conventions
    host_os = "{host_os}",

    # The host CPU architecture, isong Fuchsia conventions.
    host_arch = "{host_arch}",

    # The host tag, used to separate prebuilts in the Fuchsia source tree
    # (e.g. 'linux-x64', 'mac-x64', 'mac-arm64')
    host_tag = "{host_tag}",

    # The host tag, using underscores instead of dashes.
    host_tag_alt = "{host_tag_alt}",

    # The GCC/Clang target triple for the host operating system.
    # (e.g. x86_64-unknown-gnu-linux or aarcg64-apple-darwin).
    host_target_triple = "{host_target_triple}",

    # The Bazel platform os constraint value for the host
    # (e.g. '@platforms//os:linux' or '@platforms//os:macos'
    host_platform_os_constraint = "{host_os_constraint}",

    # The Bazel platform cpu constraint value for the host
    # (e.g. '@platforms//cpu:x86_64' or '@platforms//cpu:aarch64')
    host_platform_cpu_constraint = "{host_cpu_constraint}",

    # The RBE instance name for remote build configuration. Empty if disabled.
    rbe_instance_name = "{rbe_instance_name}",

    # The RBE container image for remote build configuration. Empty if disabled.
    rbe_container_image = "{rbe_container_image}",

    # The path to the Ninja output directory, relative to the current
    # workspace root.
    ninja_output_dir = "{ninja_output_dir}",
)
'''.format(
        host_os = host_os,
        host_arch = host_arch,
        host_target_triple = host_target_triple,
        host_tag = host_tag,
        host_tag_alt = host_tag_alt,
        host_os_constraint = host_os_constraint,
        host_cpu_constraint = host_cpu_constraint,
        rbe_instance_name = rbe_config.instance_name,
        rbe_container_image = rbe_config.container_image,
        ninja_output_dir = ninja_output_dir,
    )

    repo_ctx.file("WORKSPACE.bazel", "")
    repo_ctx.file("BUILD.bazel", "")
    repo_ctx.file("defs.bzl", defs_content)

fuchsia_build_config_repository = repository_rule(
    implementation = _fuchsia_build_config_repository_impl,
    doc = "A repository rule used to create an external repository that " +
          "contains a defs.bzl file exposing information about the current " +
          "build configuration for the Fuchsia platform build.",
    attrs = {
        "fuchsia_source_dir": attr.string(
            mandatory = False,
            doc = "Path to the Fuchsia source directory, relative to the " +
                  "current workspace.",
        ),
    },
)
