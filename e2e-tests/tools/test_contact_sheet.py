#!/usr/bin/python3 -I
"""Tests for contact-sheet.py: python3 -I e2e-tests/tools/test_contact_sheet.py"""

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "contact_sheet", Path(__file__).with_name("contact-sheet.py")
)
sheet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sheet)

PNG = b"\x89PNG\r\n\x1a\n"


def put(root, name, data):
    path = Path(root) / f"{name}.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(PNG + data)


class ContactSheet(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.now, self.then = Path(self.tmp.name, "now"), Path(self.tmp.name, "then")

    def statuses(self):
        return {name: status for name, status, _, _ in sheet.compare(self.now, self.then)}

    def test_each_side_alone_is_flagged_and_identical_bytes_are_same(self):
        put(self.now, "onboarding-full/01-welcome", b"a")
        put(self.then, "onboarding-full/01-welcome", b"a")
        put(self.now, "onboarding-full/02-components", b"b")
        put(self.then, "onboarding-full/02-components", b"c")
        put(self.now, "dictation/01-listening", b"d")
        put(self.then, "dictation/09-gone", b"e")
        self.assertEqual(
            self.statuses(),
            {
                "onboarding-full/01-welcome": "same",
                "onboarding-full/02-components": "changed",
                "dictation/01-listening": "new",
                "dictation/09-gone": "missing",
            },
        )

    def test_no_baseline_directory_means_everything_is_new(self):
        put(self.now, "dictation/01-listening", b"d")
        self.assertEqual(self.statuses(), {"dictation/01-listening": "new"})

    def test_the_page_embeds_both_images_and_says_when_there_is_no_baseline(self):
        put(self.now, "dictation/01-listening", b"d")
        page = sheet.render(sheet.compare(self.now, self.then), "T", False)
        self.assertIn("data:image/png;base64,", page)
        self.assertIn("No baseline yet", page)
        self.assertIn('class="none"', page)


if __name__ == "__main__":
    unittest.main()
