#!/usr/bin/env python3
"""Check release safety boundaries without building, signing, or contacting Apple/GitHub."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

source = Path(__file__).resolve().parent


def run(args, cwd, env=None):
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True)


with tempfile.TemporaryDirectory(prefix="bb1-release-guards-") as temporary:
    root = Path(temporary) / "repo"
    (root / "scripts").mkdir(parents=True)
    shutil.copy2(source / "release.sh", root / "scripts/release.sh")
    (root / "VERSION").write_text("0.0.1\n")
    (root / "CHANGELOG.md").write_text("## v0.0.1\n")
    # Reaching the build marks successful preflight, then stops before any real work.
    bundle = root / "scripts/bundle.sh"
    bundle.write_text('#!/bin/zsh\necho "BUILD_REACHED release=${BB1_RELEASE:-0}"\nexit 90\n')
    bundle.chmod(0o755)
    for args in [
        ["git", "init", "-q"], ["git", "add", "."],
        ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "Fixture"],
        ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "Second"],
    ]:
        assert run(args, root).returncode == 0
    base = os.environ | {"GITHUB_ACTIONS": "true", "BB1_SIGN_IDENTITY": "Developer ID Application: fixture (5PLDN9BDV4)", "BB1_NOTARIZE": "1"}
    base.pop("BB1_RELEASE", None)
    base.pop("BB1_RELEASE_TAG", None)

    def check(name, extra, expected, code=1, cwd=root):
        result = run(["scripts/release.sh", "--ci"], cwd, base | extra)
        output = result.stdout + result.stderr
        assert result.returncode == code and expected in output, (name, result.returncode, output)
        if code != 90:
            assert "BUILD_REACHED" not in output, (name, output)
        print("PASS:", name)

    check("missing signing identity fails before building", {"BB1_SIGN_IDENTITY": ""}, "CI signing is not configured")
    check("mismatched tag fails before building", {"BB1_RELEASE_TAG": "nightly-99999"}, "doesn't match")
    check("published builds cannot skip notarization", {"BB1_RELEASE_TAG": "nightly--23", "BB1_NOTARIZE": "0"}, "Published builds must be notarized")
    check("test builds never set the stable marker", {}, "BUILD_REACHED release=0", 90)
    check("stable tag sets the stable app marker", {"BB1_RELEASE_TAG": "v0.0.1"}, "BUILD_REACHED release=1", 90)
    (root / "CHANGELOG.md").write_text("# No stable entry\n")
    check("stable release needs its changelog", {"BB1_RELEASE_TAG": "v0.0.1"}, "Missing stable changelog")
    shallow = Path(temporary) / "shallow"
    assert run(["git", "clone", "--depth=1", root.as_uri(), str(shallow)], root).returncode == 0
    check("shallow history cannot reset update build numbers", {}, "Full git history is required", cwd=shallow)

    empty = os.environ | {"GITHUB_ACTIONS": "true", "RUNNER_TEMP": temporary, "GITHUB_ENV": str(Path(temporary) / "env")}
    for name in ["MACOS_CERTIFICATE", "MACOS_CERTIFICATE_PASSWORD", "NOTARY_KEY", "NOTARY_KEY_ID", "NOTARY_ISSUER"]:
        empty.pop(name, None)
    result = run([str(source / "ci-signing.sh")], root, empty)
    assert result.returncode != 0 and "Missing MACOS_CERTIFICATE secret" in result.stdout
    print("PASS: missing credentials cannot create a fallback CI identity")
