# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "compiled_tool_path", "get_fuchsia_dir", "os_exec")

def _doc_checker(ctx):
    """Runs the doc-checker tool."""

    # If a Markdown change is present (including a deletion of a markdown file),
    # check the entire project.
    if not any([f.endswith(".md") for f in ctx.scm.affected_files()]):
        return

    exe = compiled_tool_path(ctx, "doc-checker")
    res = os_exec(ctx, [exe, "--local-links-only", "--root", get_fuchsia_dir(ctx)], ok_retcodes = [0, 1]).wait()
    if res.retcode == 0:
        return
    lines = res.stdout.split("\n")
    for i in range(0, len(lines), 4):
        # The doc-checker output contains 4-line plain text entries of the
        # form:
        # """Error
        # /path/to/file:<line_number>
        # The error message
        # """
        if i + 4 > len(lines):
            break
        _, location, msg, _ = lines[i:i + 4]
        abspath = location.split(":", 1)[0]
        ctx.emit.finding(
            level = "error",
            filepath = abspath[len(ctx.scm.root) + 1:],
            message = msg + "\n\n" + "Run `fx doc-checker --local-links-only` to reproduce.",
        )

def _mdlint(ctx):
    """Runs mdlint."""
    rfc_dir = "docs/contribute/governance/rfcs/"
    affected_files = set(ctx.scm.affected_files())
    if not any([f.startswith(rfc_dir) for f in affected_files]):
        return
    mdlint = compiled_tool_path(ctx, "mdlint")
    res = os_exec(
        ctx,
        [
            mdlint,
            "--json",
            "--root-dir",
            rfc_dir,
            "--enable",
            "all",
            "--filter-filenames",
            rfc_dir,
        ],
        ok_retcodes = [0, 1],
    ).wait()
    for finding in json.decode(res.stderr):
        if finding["path"] not in ctx.scm.affected_files():
            continue
        ctx.emit.finding(
            level = "warning",
            message = finding["message"],
            filepath = finding["path"],
            line = finding["start_line"],
            end_line = finding["end_line"],
            col = finding["start_char"] + 1,
            end_col = finding["end_char"] + 1,
        )

def _codelinks(ctx):
    """Checks for certain malformatted links in source code."""
    for f, meta in ctx.scm.affected_files().items():
        # TODO(olivernewman): Files under //docs should generally reference
        # other documentation files by path (e.g. "//docs/foo/bar.md") rather
        # than URL, with some exceptions for reference docs.
        if f.startswith("docs/"):
            continue
        for num, line in meta.new_lines():
            for match in ctx.re.allmatches(
                r"(https?://)?fuchsia.googlesource.com/fuchsia/\+/(refs/heads/)?\w+/docs/(?P<path>\S+)\.md",
                line,
            ):
                repl = "https://fuchsia.dev/fuchsia-src/" + match.groups[-1]
                ctx.emit.finding(
                    level = "warning",
                    message = (
                        "Documentation links should point to fuchsia.dev rather than " +
                        "fuchsia.googlesource.com. Consider changing this to %s." % repl
                    ),
                    filepath = f,
                    line = num,
                    col = match.offset + 1,
                    end_col = match.offset + 1 + len(match.groups[0]),
                    replacements = [repl],
                )

def _rfcmeta(ctx):
    """Validates RFC metadata."""
    files = [
        f
        for f in ctx.scm.affected_files()
        # Ignore files that aren't inside the RFC directory.
        if f.startswith("docs/contribute/governance/rfcs/")
    ]
    if not files:
        return
    exe = compiled_tool_path(ctx, "rfcmeta")
    res = os_exec(ctx, [
        exe,
        "-checkout-dir",
        get_fuchsia_dir(ctx),
    ] + files).wait()

    for finding in json.decode(res.stdout):
        col = finding.get("start_char")
        end_col = finding.get("end_char")
        ctx.emit.finding(
            message = finding["message"],
            level = "warning",
            filepath = finding["path"],
            line = finding.get("start_line"),
            end_line = finding.get("end_line"),
            col = col + 1 if col else None,
            end_col = end_col + 1 if end_col else None,
        )

def register_doc_checks():
    shac.register_check(_codelinks)
    shac.register_check(shac.check(
        _doc_checker,
        # TODO(olivernewman): doc-checker has historically been run from `fx
        # format-code` even though it's not a formatter and doesn't write
        # results back to disk. Determine whether anyone depends on doc-checker
        # running with `fx format-code`, and unset `formatter = True`.
        formatter = True,
    ))
    shac.register_check(_mdlint)
    shac.register_check(_rfcmeta)
