#!/usr/bin/env python3.8
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
example usage: `./tools/fidl/scripts/syntax_coverage_check.py`
"""

from collections import namedtuple
from enum import Enum
from pathlib import Path
from pprint import pprint
import re
import os

TestCase = namedtuple('TestCase', ['suite', 'name'])

IGNORED_FILES = {
    # syntax agnostic/specific
    'c_generator_tests.cc',
    'new_syntax_converter_tests.cc',
    'flat_ast_tests.cc',
    'new_syntax_tests.cc',
    'recursion_detector_tests.cc',
    'reporter_tests.cc',
    'type_alias_tests.cc',
    'virtual_source_tests.cc',
    'utils_tests.cc',
    # will be updated along with formatter/linter work
    'formatter_tests.cc',
    'json_findings_tests.cc',
    'lint_findings_tests.cc',
    'lint_tests.cc',
    'visitor_unittests.cc',
}

# success tests that don't have any of the convert macros in them
GOOD_TEST_ALLOWLIST = {
    # these are manually duplicated
    TestCase('ParsingTests', 'GoodAttributeValueHasCorrectContents'),
    TestCase('ParsingTests', 'GoodAttributeValueHasCorrectContentsOld'),
    TestCase('ParsingTests', 'GoodMultilineCommentHasCorrectContents'),
    TestCase('ParsingTests', 'GoodMultilineCommentHasCorrectContentsOld'),
    TestCase('SpanTests', 'GoodParseTest'),
    TestCase('SpanTests', 'GoodParseTestOld'),
    # these test cases run across both syntaxes without using the macro
    TestCase('TypesTests', 'GoodRootTypesWithNoLibraryInLookup'),
    TestCase('TypesTests', 'GoodRootTypesWithSomeLibraryInLookup'),
    TestCase('TypesTests', 'GoodHandleSubtype'),
    TestCase('TypesTests', 'GoodRights'),
    # only applies to the new syntax
    TestCase('ParsingTests', 'GoodSingleConstraint'),
}

FUCHSIA_DIR = os.environ['FUCHSIA_DIR']

WHITE = '\033[1;37m'
NC = '\033[0m'

ALLOWED_PREFIXES = ['Good', 'Bad', 'Warn']


def print_color(s, color):
    print('{}{}{}'.format(color, s, NC))


def get_all_test_files():
    test_dir = Path(FUCHSIA_DIR) / 'zircon/system/utest/fidl-compiler'
    for file in test_dir.iterdir():
        if file.suffix == '.cc' and file.name not in IGNORED_FILES:
            yield file


if __name__ == '__main__':
    unlabeled_tests = set()
    for path in get_all_test_files():
        print_color(f'analyzing file: {path.name}', WHITE)
        with open(path, 'r') as f:
            old_to_errors = {}
            new_to_errors = {}
            test_to_notes = {}

            current = TestCase('', '')
            is_converted = False
            errors = []
            note = []
            in_note = False
            for line in f:
                # start of a new test case
                if line.startswith('TEST('):
                    if current.name.startswith('Good'):
                        if not is_converted and current not in GOOD_TEST_ALLOWLIST:
                            print(f'  {current.name}')
                            # pass
                    elif current.name.startswith('Bad'):
                        lookup = old_to_errors if current.name.endswith(
                            'Old') else new_to_errors
                        lookup[current.name] = errors
                        if note:
                            test_to_notes[current.name] = '\n'.join(note)

                    # reset test case state
                    suite = line[line.find('(') + 1:line.find(',')]
                    name = line[line.find(',') + 2:line.find(')')]
                    current = TestCase(suite, name)
                    is_converted = False
                    errors = []
                    note = []
                    in_note = False
                    if not any(current.name.startswith(p)
                               for p in ALLOWED_PREFIXES):
                        unlabeled_tests.add(current)
                    continue

                if 'ASSERT_COMPILED_AND_CONVERT(' in line or 'ASSERT_COMPILED_AND_CONVERT_WITH_DEP(' in line:
                    assert not current.name.startswith('Bad')
                    is_converted = True
                elif in_note:
                    assert not current.name.startswith('Good')
                    if line.lstrip().startswith('// '):
                        note.append(line.strip())
                    else:
                        in_note = False
                elif 'NOTE(fxbug.dev/72924)' in line:
                    assert not current.name.startswith('Good')
                    note.append(line.strip())
                    in_note = True
                else:
                    errors.extend(re.findall('fidl::(Err\w+)', line))
                    errors.extend(re.findall('fidl::(Warn\w+)', line))

            # handle the last test
            if current.name.startswith('Good'):
                if not is_converted and current not in GOOD_TEST_ALLOWLIST:
                    print(f'  {current.name}')
                    # pass
            elif current.name.startswith('Bad'):
                lookup = old_to_errors if current.name.endswith(
                    'Old') else new_to_errors
                lookup[current.name] = errors
                if note:
                    test_to_notes[current.name] = '\n'.join(note)

        # strip the Old suffix first
        # old_tests = set(t[:-3] for t in old_to_errors.keys())
        # new_tests = set(t for t in new_to_errors.keys())
        # for test_name in old_tests | new_tests:
        #     if test_name not in old_tests:
        #         print(f' missing old: {test_name}Old')
        #     elif test_name not in new_tests:
        #         print(f' missing new: {test_name}')

    if unlabeled_tests:
        print('found unlabeled tests:')
        pprint(unlabeled_tests)
