import tempfile
import unittest
from pathlib import Path

from check_readme_metadata import check


class ReadmeMetadataTest(unittest.TestCase):
    def test_repository_counts_match_metadata(self):
        self.assertEqual(check(), [])

    def test_stale_workspace_count_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs" / "adr").mkdir(parents=True)
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["crates/a", "crates/b"]\n',
                encoding="utf-8",
            )
            (root / "docs" / "adr" / "0001-first.md").write_text(
                "- **Status:** Accepted\n", encoding="utf-8"
            )
            (root / "README.md").write_text(
                "| Workspace crates | 1 members |\n"
                "| ADRs | 1 accepted architecture decision records |\n"
                "| Docs | 0 design docs |\n"
                "## Documentation\n\n"
                "| [`docs/adr/`](docs/adr/) | ADR index. |\n",
                encoding="utf-8",
            )

            errors = check(root)

        self.assertTrue(any("Workspace crates says 1" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
