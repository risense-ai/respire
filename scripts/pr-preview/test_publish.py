"""Synthetic tests for the one-time preview fixture publication."""
import copy
import unittest
from unittest.mock import patch

import publish


class FixtureBodyTests(unittest.TestCase):
    head = 'a' * 40
    url = 'https://github.com/risense-ai/respire/blob/' + 'b' * 40 + '/pr-17/' + head + '/'

    def setUp(self):
        self.fixture = {
            'state': 'open', 'draft': True, 'body': publish.FIXTURE_BODY,
            'head': {'repo': {'full_name': publish.REPO}, 'sha': self.head,
                     'ref': 'test/client2-preview-fixture'},
            'base': {'ref': 'main'}, 'labels': [{'name': 'ui-preview'}],
        }

    def test_replaces_only_the_exact_opt_in_body_then_reads_it_back(self):
        expected = publish.fixture_body(self.fixture, self.head, self.url)
        self.assertIsNotNone(expected)
        for name in ('en.png', 'zh.png', 'en.mp4', 'zh.mp4'):
            self.assertIn(self.url + name, expected)
        final = dict(self.fixture, body=expected)
        with patch.object(publish, 'api', side_effect=[self.fixture, {}, final]) as api:
            publish.publish_fixture_body(17, self.head, self.url)
        self.assertEqual(api.call_args_list[1].args, ('PATCH', 'pulls/17', {'body': expected}))
        self.assertEqual(api.call_args_list[-1].args, ('GET', 'pulls/17'))

    def test_never_mutates_an_ordinary_pr_or_a_changed_fixture(self):
        cases = [
            dict(self.fixture, body='Human text\n' + publish.FIXTURE_BODY),
            dict(self.fixture, body=None),
            dict(self.fixture, draft=False),
            dict(self.fixture, labels=[]),
        ]
        ordinary = copy.deepcopy(self.fixture)
        ordinary['head']['ref'] = 'feat/client2-settings'
        cases.append(ordinary)
        stale = copy.deepcopy(self.fixture)
        stale['head']['sha'] = 'c' * 40
        cases.append(stale)
        for case in cases:
            with patch.object(publish, 'api', return_value=case) as api:
                publish.publish_fixture_body(17, self.head, self.url)
            api.assert_called_once_with('GET', 'pulls/17')

    def test_does_not_claim_publication_without_matching_readback(self):
        with patch.object(publish, 'api', side_effect=[self.fixture, {}, self.fixture]):
            with self.assertRaisesRegex(ValueError, 'not verified'):
                publish.publish_fixture_body(17, self.head, self.url)


if __name__ == '__main__':
    unittest.main()
