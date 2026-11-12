import tempfile
import unittest
from pathlib import Path

from check_release_notes import check


class ReleaseNotesTest(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8"
        )
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n"
            "## [1.2.3] - 2026-11-10\n\n"
            "### Added\n\n- New feature.\n\n"
            "### Migration\n\n- None.\n\n"
            "## [Unreleased]\n",
            encoding="utf-8",
        )

    def tearDown(self):
        self.tempdir.cleanup()

    def test_complete_versioned_notes_pass(self):
        self.assertEqual(check(self.root, "v1.2.3"), [])

    def test_missing_migration_notes_fail(self):
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        changelog = changelog.replace("### Migration\n\n- None.\n", "")
        (self.root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")

        self.assertTrue(any("Migration" in error for error in check(self.root, "v1.2.3")))

    def test_empty_migration_section_fails(self):
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        changelog = changelog.replace("- None.", "<!-- migration note -->")
        (self.root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")

        self.assertTrue(any("Migration" in error for error in check(self.root, "v1.2.3")))

    def test_missing_versioned_section_fails(self):
        (self.root / "CHANGELOG.md").write_text("# Changelog\n## [Unreleased]\n")

        self.assertTrue(any("add a `## [1.2.3]`" in error for error in check(self.root, "v1.2.3")))

    def test_tag_must_match_workspace_version(self):
        errors = check(self.root, "v1.2.4")

        self.assertTrue(any("does not match workspace version" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
