#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import subprocess
import json
import unittest
import os
from unittest import mock

from typing import List

import util
import submit


# Labels for CQ+1 and CQ+2 respectively.
_CQ_PLUS1_LABEL = {'Commit-Queue': {'recommended': {'user_id': 1}}}
_CQ_PLUS2_LABEL = {'Commit-Queue': {'approved': {'user_id': 1}}}


class TestSubmit(unittest.TestCase):

    def test_change_from_json(self) -> None:
        parsed_json = json.loads(
            r"""
      {
        "unresolved_comment_count": 0,
        "has_review_started": true,
        "mergeable": true,
        "id": "fuchsia~master~I12345",
        "branch": "master",
        "subject": "Implement the thing.",
        "status": "NEW",
        "labels": {
            "Testability-Review": { "approved": { "_account_id": 123 } },
            "Commit-Message-has-tags": { "approved": { "_account_id": 123 } },
            "No-Patches-File": { "approved": { "_account_id": 123 } },
            "Commit-Queue": { "optional": true },
            "Code-Review": { "approved": { "_account_id": 123 } }
        },
        "problems": [],
        "total_comment_count": 0,
        "change_id": "I12345",
        "current_revision": "aaaabbbbccccddd",
        "submittable": true,
        "project": "fuchsia"
      }
    """)
        change = submit.Change.from_json(parsed_json)
        submit.print_changes([change])

    def test_print_changes(self) -> None:
        submit.print_changes(
            [
                submit.Change('abc', {'subject': 'Test'}),
                submit.Change('abc', {'subject': 'Test'}),
                submit.Change('abc', {'subject': 'Test'}),
            ])

    def test_cq_votes(self) -> None:
        c = submit.Change('abc', {'labels': _CQ_PLUS2_LABEL})
        assert c.cq_votes() == 2
        c = submit.Change('abc', {'labels': _CQ_PLUS1_LABEL})
        assert c.cq_votes() == 1
        c = submit.Change('abc', {})
        assert c.cq_votes() == 0

    def test_unresolved_comment(self) -> None:
        c = submit.Change('abc', {'labels': _CQ_PLUS2_LABEL})
        assert not c.has_unresolved_comments()
        c = submit.Change('abc', {'unresolved_comment_count': 0})
        assert not c.has_unresolved_comments()
        c = submit.Change('abc', {'unresolved_comment_count': 2})
        assert c.has_unresolved_comments()

    def test_submit_cls_empty(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        success = submit.submit_changes(util.FakeClock(), server, [])
        assert success

    def test_submit_cls_single(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        server.fetch_change.side_effect = [
            # Initially, not on the queue.
            submit.Change('42', {}),
            # On the queue.
            submit.Change('42', {'labels': _CQ_PLUS2_LABEL}),
            submit.Change('42', {'labels': _CQ_PLUS2_LABEL}),
            # Retry.
            submit.Change('42', {}),
            # On the queue.
            submit.Change('42', {'labels': _CQ_PLUS2_LABEL}),
            # Submitted.
            submit.Change('42', {'status': 'MERGED'}),
        ]
        success = submit.submit_changes(
            util.FakeClock(),
            server, [submit.Change('42', {'submittable': True})],
            num_retries=2)
        assert success

        # Should have added to CQ twice.
        server.set_cq_state.assert_called_with('42', 2)
        assert server.set_cq_state.call_count == 2

    def test_submit_cls_too_many_retries(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        server.fetch_change.side_effect = [
            # Initially, not on the queue.
            submit.Change('42', {}),
            # When we next poll, we have been kicked out of the queue again. We should retry.
            submit.Change('42', {}),
            # And again. Retry again.
            submit.Change('42', {}),
            # And again. We should give up.
            submit.Change('42', {}),
        ]
        success = submit.submit_changes(
            util.FakeClock(),
            server, [submit.Change('42', {'submittable': True})],
            num_retries=2)
        assert not success

        # Should have added to CQ twice.
        server.set_cq_state.assert_called_with('42', 2)
        assert server.set_cq_state.call_count == 3

    def test_submit_existing_submit_counts_as_retry(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        server.fetch_change.side_effect = [
            # Initially, the change is being submitted.
            submit.Change('42', {'labels': _CQ_PLUS2_LABEL}),
            # When we next poll, we have been kicked out of the queue. We should give up.
            submit.Change('42', {}),
        ]
        submit.submit_changes(
            util.FakeClock(),
            server, [submit.Change('42', {'submittable': True, 'labels': _CQ_PLUS2_LABEL})],
            num_retries=0)

        # Should have not have been added to CQ, other than the initial existing attempt.
        assert server.set_cq_state.call_count == 0

    def test_submit_cls_upgrade_to_approved(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        server.fetch_change.side_effect = [
            # Initially, on the queue as recommended.
            submit.Change('42', {'labels': _CQ_PLUS1_LABEL}),
            # Submitted.
            submit.Change('42', {'status': 'MERGED'}),
        ]
        submit.submit_changes(
            util.FakeClock(), server,
            [submit.Change('42', {'submittable': True})])

        # Should have added to CQ twice.
        server.set_cq_state.assert_called_with('42', 2)
        assert server.set_cq_state.call_count == 1

    def test_get_change_dependencies(self) -> None:
        parsed_json = json.loads(
            r"""
      {"changes": [
        {"change_id": "I0001"},
        {"change_id": "I0002"},
        {"change_id": "I0003"},
        {"change_id": "I0004"},
        {"change_id": "I0005"}
      ]}
    """)
        with mock.patch.object(submit.gerrit_util, 'GetRelatedChanges',
                               return_value=parsed_json) as mock_method:
            server = submit.GerritServer('some-host')
            self.assertEqual(
                server.get_change_dependencies('I0003'),
                ['I0003', 'I0004', 'I0005'])

    def test_get_change_dependencies_no_changes(self) -> None:
        parsed_json = json.loads('{}')
        with mock.patch.object(submit.gerrit_util, 'GetRelatedChanges',
                               return_value=parsed_json) as mock_method:
            server = submit.GerritServer('some-host')
            self.assertEqual(server.get_change_dependencies('I0003'), ['I0003'])

    def test_is_valid_change_id(self) -> None:
        self.assertTrue(submit.is_valid_change_id('1'))
        self.assertTrue(submit.is_valid_change_id('123'))
        self.assertTrue(
            submit.is_valid_change_id(
                'I609f446e6721dd95624939dd041189052054fb83'))

        self.assertFalse(submit.is_valid_change_id(''))
        self.assertFalse(submit.is_valid_change_id('0'))
        self.assertFalse(submit.is_valid_change_id('-12312'))
        self.assertFalse(submit.is_valid_change_id('I'))
        self.assertFalse(submit.is_valid_change_id('I1111111'))
        self.assertFalse(
            submit.is_valid_change_id(
                '609f446e6721dd95624939dd041189052054fb83'))

    def test_submit_cls_pretest(self) -> None:
        server = mock.Mock(spec=submit.GerritServer)
        server.fetch_change.side_effect = [
            submit.Change('42', {'status': 'NEW'}),
            submit.Change('42', {'status': 'MERGED'}),
            submit.Change('17', {'status': 'NEW'}),
            submit.Change('17', {'status': 'MERGED'}),
        ]
        submit.submit_changes(
            util.FakeClock(), server, [
                submit.Change('42', {'submittable': True}),
                submit.Change('17', {'submittable': True}),
            ])

        # Should have added to CQ twice.
        server.set_cq_state.assert_has_calls(
            [
                mock.call('17', 1),
                mock.call('42', 2),
                mock.call('17', 2),
            ])

    def test_ensure_changes_submittable(self) -> None:
        needs_votes_cl = submit.Change(
            'abc', {
                'status': 'NEW',
                'submittable': False
            })
        unresolved_cl = submit.Change(
            'abc', {
                'status': 'NEW',
                'submittable': True,
                'unresolved_comment_count': 2
            })
        ready_cl = submit.Change('abc', {'status': 'NEW', 'submittable': True})

        # These should work without exception.
        submit.ensure_changes_submittable([])
        submit.ensure_changes_submittable([ready_cl])
        submit.ensure_changes_submittable([ready_cl, ready_cl])
        submit.ensure_changes_submittable(
            [unresolved_cl], abort_on_unresolved_comments=False)

        # Expect these to abort.
        self.assertRaises(
            submit.SubmitError,
            lambda: submit.ensure_changes_submittable([needs_votes_cl]))
        self.assertRaises(
            submit.SubmitError, lambda: submit.ensure_changes_submittable(
                [ready_cl, needs_votes_cl]))
        self.assertRaises(
            submit.SubmitError, lambda: submit.ensure_changes_submittable(
                [needs_votes_cl, ready_cl]))
        self.assertRaises(
            submit.SubmitError,
            lambda: submit.ensure_changes_submittable([unresolved_cl]))

    def test_submit_cl_error_description(self) -> None:
        c = submit.Change(
            'abc', {
                'status': 'NEW',
                'submittable': True,
                'unresolved_comment_count': 2
            })
        print(c.status.submit_error_description())
        assert c.status.submit_error_description(
        ) == "CL has unresolved comments"

        c = submit.Change('42', {'status': 'NEW', 'submittable': False})
        assert c.status.submit_error_description() == "CL is missing votes"

        c = submit.Change('42', {'status': 'NEW', 'submittable': True})
        assert c.status.submit_error_description() is None


class TestTypes(unittest.TestCase):

    def test_types(self) -> None:
        self_dir = os.path.dirname(os.path.realpath(__file__))
        subprocess.check_call(['env', 'python3', '-m', 'mypy', self_dir])


if __name__ == '__main__':
    unittest.main()
