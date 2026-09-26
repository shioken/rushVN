"""Regression checks for scoped third-party attribution exceptions."""
import hashlib
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "privacy", Path(__file__).resolve().parents[1] / "scripts/check_privacy.py")
PRIVACY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRIVACY)


class LicenseNoticePrivacyTests(unittest.TestCase):
    path = "THIRD_PARTY_NOTICES.md"
    # Construct an address outside the example allowlist without storing a real address.
    notice = b"Copyright Upstream <author" + b"@" + b"upstream.org>\n"

    def manifest(self, data):
        return {self.path: hashlib.sha256(data).hexdigest()}

    def test_reviewed_notice_preserves_attribution(self):
        self.assertEqual(PRIVACY.issues(self.path, self.notice, {}, self.manifest(self.notice)), [])

    def test_modified_notice_requires_review(self):
        findings = PRIVACY.issues(self.path, self.notice + b"changed", {}, self.manifest(self.notice))
        self.assertTrue(any("license notice changed" in issue for issue in findings))
        self.assertIn("non-example email address", findings)

    def test_other_paths_cannot_use_license_exception(self):
        path = "src/unrelated.rs"
        manifest = {path: hashlib.sha256(self.notice).hexdigest()}
        self.assertIn("non-example email address", PRIVACY.issues(path, self.notice, {}, manifest))

    def test_notice_without_manifest_is_checked(self):
        self.assertIn("non-example email address", PRIVACY.issues(self.path, self.notice, {}))

    def test_reviewed_notice_still_checks_secrets_and_home_paths(self):
        examples = [
            (b"-----BEGIN " + b"PRIVATE KEY-----", "private key"),
            (b"ghp_" + b"x" * 30, "access token"),
            (b"/Users/" + b"private-person/file", "personal home path"),
            (b"/home/" + b"private-person/file", "personal home path"),
        ]
        for data, expected in examples:
            with self.subTest(expected=expected, data=data):
                self.assertIn(expected, PRIVACY.issues(self.path, data, {}, self.manifest(data)))

    def test_crate_home_source_url_is_not_a_local_path(self):
        data = b"https://crates.io/api/v1/crates/home/0.5.12/download"
        self.assertEqual(PRIVACY.issues("docs/sources.md", data, {}), [])


if __name__ == "__main__":
    unittest.main()
