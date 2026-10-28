import tempfile
import unittest
from pathlib import Path

from check_markdown_links import anchors, check


class MarkdownLinksTest(unittest.TestCase):
    def test_anchors_and_duplicate_headings(self):
        self.assertEqual(anchors("# Hello, `World`!\n# Hello, `World`!\n<a id=\"custom\"></a>"),
                         {"hello-world", "hello-world-1", "custom"})

    def test_relative_links_anchors_and_ignored_content(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "sub").mkdir()
            (root / "sub" / "other file.md").write_text("# A heading\n", encoding="utf-8")
            (root / "README.md").write_text(
                "# Home\n[ok](sub/other%20file.md#a-heading)\n"
                "[self](#home) [external](https://example.com/missing)\n"
                "`[inline](missing)`\n```md\n[example](missing)\n```\n"
                "[bad](missing.md)\n[bad anchor](sub/other%20file.md#wrong)\n",
                encoding="utf-8")
            _, errors = check(root)
            self.assertEqual(len(errors), 2)
            self.assertIn("README.md:8: missing target", errors[0])
            self.assertIn("README.md:9: missing anchor", errors[1])


if __name__ == "__main__":
    unittest.main()
