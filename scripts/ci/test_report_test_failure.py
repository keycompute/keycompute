import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("report", Path(__file__).with_name("report-test-failure.py"))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class FailureReportTest(unittest.TestCase):
    def test_failure_keeps_assertion_context(self):
        detail = report.excerpt("compile line\ntest scope ... FAILED\n\nthread 'scope' panicked at test.rs:9\nassertion failed\nleft: 1\nright: 2\n")
        self.assertIn("left: 1", detail)
        self.assertIn("right: 2", detail)

    def test_annotation_cannot_inject_workflow_commands(self):
        self.assertEqual(report.annotation("%\r\n::notice::x"), "%25%0D%0A::notice::x")

    def test_bounded_fallback_and_empty_input(self):
        self.assertLessEqual(len(report.excerpt("x" * 20000)), 16000)
        self.assertEqual(report.excerpt(""), "")


if __name__ == "__main__":
    unittest.main()
