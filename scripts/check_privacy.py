#!/usr/bin/env python3
"""Check staged files, the worktree, or reachable commits without printing secrets."""
import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import PurePosixPath

IDENTITY = "rustVN Contributors <contributors@rustvn.invalid>"
HISTORICAL_IDENTITIES = {IDENTITY, "rushVN Contributors <contributors@rushvn.invalid>"}
LICENSE_NOTICE_PATHS = {"THIRD_PARTY_NOTICES.md", "licenses/upstream.json"}
EMAIL = re.compile(rb"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)")
RULES = {
    "private conversation URL": rb"https?://chatgpt\.com/(?:c|share)/[a-zA-Z0-9-]+",
    "personal home path": rb"(?<![A-Za-z0-9])(?:/Users/|/home/)[a-zA-Z0-9_.-]+|[A-Za-z]:\\Users\\[a-zA-Z0-9_.-]+",
    "private key": rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
    "access token": rb"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,})",
}


def git(*args):
    return subprocess.check_output(["git", *args])


def issues(path, data, assets, licenses=None):
    result = []
    name = PurePosixPath(path).name.lower()
    if (name.startswith((".env", ".newsrc", ".authinfo")) and name != ".env.example"
        or name == "window.ron" or name.endswith((".db", ".sqlite", ".sqlite3", ".log", ".eml", ".nntp", ".key", ".pfx"))
        or re.search(r"\.(?:db|sqlite3?)-(?:wal|shm|journal)$", name)):
        result.append("local user data or credential file")
    if path in assets:
        if hashlib.sha256(data).hexdigest() != assets[path]:
            result.append("asset changed: review content before updating public-assets.json")
        return result
    if name.endswith((".pem", ".p12")):
        result.append("unreviewed certificate or key container")
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        result.append("unreviewed binary asset")
        return result
    for rule, pattern in RULES.items():
        if re.search(pattern, data):
            result.append(rule)
    # Only reviewed, byte-identical third-party notices may retain upstream emails.
    # This does not bypass credential, home-path, or other checks above.
    if path in LICENSE_NOTICE_PATHS and path in (licenses or {}):
        if hashlib.sha256(data).hexdigest() == licenses[path]:
            return result
        result.append("license notice changed: review upstream attribution before updating hash")
    for match in EMAIL.finditer(data):
        domain = match[1].decode("ascii").lower()
        if domain not in {"test", "fixture", "localhost", "example.com", "example.org", "example.net"} and not domain.endswith((".invalid", ".test", ".example")):
            result.append("non-example email address")
            break
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--history", action="store_true")
    mode.add_argument("--worktree", action="store_true")
    args = parser.parse_args()
    # Use the staged manifest too, so unstaged changes cannot silently waive a finding.
    def read_manifest(path, revision=None):
        if args.worktree:
            from pathlib import Path
            return json.loads(Path(path).read_text())
        spec = f"{revision or ''}:{path}"
        found = subprocess.run(["git", "show", spec], capture_output=True)
        return json.loads(found.stdout) if found.returncode == 0 else {}

    assets = read_manifest("scripts/public-assets.json")
    licenses = read_manifest("scripts/license-notices.json")
    failures = []
    seen = set()
    if args.history:
        revisions = git("rev-list", "--all").decode().splitlines()
    else:
        revisions = [None]
        for kind in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"]:
            if not git("var", kind).decode().startswith(IDENTITY + " "):
                failures.append((kind, "use the repository's anonymous commit identity"))
    for revision in revisions:
        if revision:
            assets = read_manifest("scripts/public-assets.json", revision)
            licenses = read_manifest("scripts/license-notices.json", revision)
            author, committer, message = git("show", "-s", "--format=%an <%ae>%n%cn <%ce>%n%B", revision).decode().split("\n", 2)
            if author not in HISTORICAL_IDENTITIES or committer not in HISTORICAL_IDENTITIES:
                failures.append((revision[:12], "personal commit identity"))
            for issue in issues("commit-message", message.encode(), assets, licenses):
                failures.append((revision[:12], issue))
            entries = git("ls-tree", "-rz", revision).split(b"\0")
        elif args.worktree:
            from pathlib import Path
            paths = git("ls-files", "--cached", "--others", "--exclude-standard", "-z").split(b"\0")
            for raw_path in sorted(set(paths)):
                if not raw_path:
                    continue
                path = raw_path.decode()
                if not Path(path).is_file():
                    continue
                data = Path(path).read_bytes()
                seen.add((path, hashlib.sha256(data).hexdigest()))
                for issue in issues(path, data, assets, licenses):
                    failures.append((path, issue))
            continue
        else:
            entries = git("ls-files", "--stage", "-z").split(b"\0")
        for entry in entries:
            if not entry:
                continue
            meta, raw_path = entry.split(b"\t", 1)
            fields = meta.split()
            oid = (fields[2] if revision else fields[1]).decode()
            path = raw_path.decode()
            if fields[0] == b"160000":
                failures.append((path, "submodule requires a separate privacy review"))
                continue
            snapshot_key = (path, oid, assets.get(path), licenses.get(path))
            if snapshot_key in seen:
                continue
            seen.add(snapshot_key)
            for issue in issues(path, git("cat-file", "blob", oid), assets, licenses):
                failures.append((path, issue))
    for path, issue in failures:
        print(f"FAIL {path}: {issue}", file=sys.stderr)
    if failures:
        return 1
    print(f"Privacy patterns checked: {len(seen)} file versions; no findings.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
