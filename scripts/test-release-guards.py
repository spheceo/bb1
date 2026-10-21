#!/usr/bin/env python3
"""Check release safety boundaries without building, signing, or contacting Apple/GitHub."""
import os
import hashlib
import json
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

    # An interrupted artifact upload must leave a draft, never a visible partial release.
    dist = root / "dist"
    dist.mkdir()
    filenames = ["bb1-nightly--23.dmg", "bb1-0.0.1-build2.zip"]
    for filename in filenames:
        (dist / filename).write_bytes(b"Fixture deliverable")
    (dist / "SHA256SUMS").write_text("".join(
        f"{hashlib.sha256((dist / filename).read_bytes()).hexdigest()}  {filename}\n"
        for filename in filenames
    ))
    commands = Path(temporary) / "bin"
    commands.mkdir()
    mock = commands / "gh"
    mock.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
state = pathlib.Path(os.environ["MOCK_STATE"])
with open(os.environ["MOCK_LOG"], "a") as log:
    log.write(json.dumps(args) + "\\n")
if args[:2] == ["release", "view"]:
    if not state.exists(): sys.exit(1)
    draft = state.read_text() == "draft"
    print(str(draft).lower() if "--jq" in args else json.dumps({"isDraft": draft}))
elif args[:2] == ["release", "create"]:
    state.write_text("draft")
elif args[:2] == ["release", "upload"]:
    if os.environ.get("MOCK_FAIL_UPLOAD") == "1": sys.exit(1)
elif args[:2] == ["release", "edit"]:
    state.write_text("public")
else: sys.exit(2)
''')
    mock.chmod(0o755)
    state, log = Path(temporary) / "state", Path(temporary) / "commands"
    publish_env = os.environ | {"PATH": str(commands) + os.pathsep + os.environ["PATH"], "MOCK_STATE": str(state), "MOCK_LOG": str(log), "BB1_RELEASE_TAG": "nightly--23"}
    result = run([str(source / "ci-publish.sh")], root, publish_env | {"MOCK_FAIL_UPLOAD": "1"})
    assert result.returncode != 0 and state.read_text() == "draft"
    assert all(json.loads(line)[:2] != ["release", "edit"] for line in log.read_text().splitlines())
    print("PASS: upload failure leaves the release hidden as a draft")
    result = run([str(source / "ci-publish.sh")], root, publish_env)
    assert result.returncode == 0 and state.read_text() == "public", result.stderr
    edits = [json.loads(line) for line in log.read_text().splitlines() if json.loads(line)[:2] == ["release", "edit"]]
    assert len(edits) == 1 and "--prerelease" in edits[0] and "--latest=false" in edits[0]
    print("PASS: draft retry publishes a nightly without replacing the stable release")
    result = run([str(source / "ci-publish.sh")], root, publish_env)
    assert result.returncode != 0 and "Refusing to replace" in result.stderr
    print("PASS: an already public release cannot be overwritten")
