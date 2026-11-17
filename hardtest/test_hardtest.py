import unittest

from hardtest import core


class ClassificationTests(unittest.TestCase):
    def test_known_parser_limit_requires_exact_status_and_diagnostic(self):
        self.assertTrue(core.parser_limit(2, b'', b'nsh: shell syntax nesting limit exceeded\n'))
        self.assertFalse(core.parser_limit(0, b'', b'nsh: shell syntax nesting limit exceeded\n'))
        self.assertFalse(core.parser_limit(2, b'x', b'nsh: shell syntax nesting limit exceeded\n'))

    def test_wrong_exit_is_not_a_crash(self):
        self.assertEqual(core.classify(3, b'', b'', 0, b'', b''), 'wrong_exit_status')

    def test_signal_is_crash(self):
        self.assertEqual(core.classify(-11, b'', b'', 0, b'', b''), 'crash')

    def test_unexpected_stderr(self):
        self.assertEqual(core.classify(0, b'ok', b'error', 0, b'ok', b''), 'unexpected_stderr')

    def test_wrong_output(self):
        self.assertEqual(core.classify(0, b'bad', b'', 0, b'ok', b''), 'wrong_output')

    def test_critical_failure_caps_score(self):
        score = core.score({'correctness': 0.9, 'stability': 1, 'memory': 1, 'performance': 1, 'interactive': 1}, critical=True)
        self.assertLessEqual(score, 3000)

    def test_score_is_deterministic(self):
        score = core.score({'correctness': 1, 'stability': 1, 'memory': 1, 'performance': 1, 'interactive': 1}, critical=False)
        self.assertEqual(score, 10000)


if __name__ == '__main__':
    unittest.main()
