#!/usr/bin/env python3
"""Execute release shell entrypoints with build/network commands stubbed out."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseShellTests(unittest.TestCase):
    def run_idc_settings(self, **overrides):
        workflow = (ROOT / ".github/workflows/build_push_to_idc.yml").read_text()
        script = workflow.split("        run: |\n", 1)[1].split("\n  candidates:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = Path(temporary_directory)
            origin = temporary / "origin.git"
            checkout = temporary / "checkout"
            subprocess.run(["git", "init", "--bare", str(origin)], check=True,
                           capture_output=True)
            subprocess.run(["git", "init", "-b", "main", str(checkout)], check=True,
                           capture_output=True)
            for key, value in (("user.name", "Release Test"),
                               ("user.email", "release-test@example.invalid")):
                subprocess.run(["git", "-C", str(checkout), "config", key, value], check=True)
            marker = checkout / "marker"
            marker.write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(checkout), "add", "marker"], check=True)
            subprocess.run(["git", "-C", str(checkout), "commit", "-m", "base"],
                           check=True, capture_output=True)
            base_sha = subprocess.check_output(
                ["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
            subprocess.run(["git", "-C", str(checkout), "remote", "add", "origin", str(origin)],
                           check=True)
            subprocess.run(["git", "-C", str(checkout), "push", "origin", "main"],
                           check=True, capture_output=True)
            subprocess.run(["git", "-C", str(checkout), "switch", "-c", "moi-dev"],
                           check=True, capture_output=True)
            marker.write_text("moi-dev\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(checkout), "commit", "-am", "moi-dev"],
                           check=True, capture_output=True)
            moi_dev_sha = subprocess.check_output(
                ["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
            subprocess.run(["git", "-C", str(checkout), "push", "origin", "moi-dev"],
                           check=True, capture_output=True)
            subprocess.run(["git", "-C", str(checkout), "switch", "main"],
                           check=True, capture_output=True)
            main_sha = subprocess.check_output(
                ["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
            source_ref = overrides.pop("SOURCE_REF", "main")
            if source_ref == "__base_sha__":
                source_ref = base_sha
            env = {
                **os.environ,
                "DEFAULT_BRANCH": "main",
                "SOURCE_REF": source_ref,
                "IDC_REGISTRY": "registry.example:5000",
                "IDC_IMAGE": "registry.example:5000/team/astra",
                "IDC_RUNNER": "idc-amd64",
                "GITHUB_REF": "refs/heads/main",
                "GITHUB_SHA": main_sha,
                "GITHUB_RUN_ID": "123",
                "GITHUB_OUTPUT": "/dev/stdout",
                **overrides,
            }
            result = subprocess.run(["bash", "-c", script], env=env, cwd=checkout,
                                    capture_output=True, text=True)
            return result, {"main": main_sha, "moi-dev": moi_dev_sha, "base": base_sha}

    def test_idc_build_identity_and_runner(self):
        result, revisions = self.run_idc_settings()
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(outputs["controller_sha"], revisions["main"])
        self.assertEqual(outputs["source_sha"], revisions["main"])
        self.assertRegex(outputs["image_version"], r"^idc-\d{8}T\d{6}Z-" + revisions["main"] + r"-123-amd64$")
        self.assertEqual(json.loads(outputs["matrix"]), {"include": [
            {"platform": "linux/amd64", "runner": ["self-hosted", "idc-amd64"],
             "slug": "linux-amd64"}]})

    def test_idc_resolves_moi_dev_and_allowed_historical_commit(self):
        result, revisions = self.run_idc_settings(SOURCE_REF="moi-dev")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("source_sha=" + revisions["moi-dev"], result.stdout)
        result, revisions = self.run_idc_settings(SOURCE_REF="__base_sha__")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("source_sha=" + revisions["base"], result.stdout)

    def test_idc_rejects_non_main_controller_and_arbitrary_ref(self):
        result, _ = self.run_idc_settings(GITHUB_REF="refs/heads/moi-dev")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Run this workflow from main", result.stdout)
        result, _ = self.run_idc_settings(SOURCE_REF="feature/test")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source_ref must be main, moi-dev, or a full commit SHA", result.stdout)

    def test_idc_missing_configuration_stops_before_build(self):
        for key in ("IDC_REGISTRY", "IDC_IMAGE", "IDC_RUNNER"):
            with self.subTest(key=key):
                result, _ = self.run_idc_settings(**{key: ""})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Missing required IDC configuration: " + key, result.stdout)
                self.assertNotIn("matrix=", result.stdout)

    def test_idc_rejects_wrong_or_tagged_repository(self):
        for image in ("docker.io/team/astra", "registry.example:5000/",
                      "registry.example:5000/team/astra:latest",
                      "registry.example:5000/team/astra@sha256:abc",
                      "registry.example:5000/team/astra bad"):
            with self.subTest(image=image):
                result, _ = self.run_idc_settings(IDC_IMAGE=image)
                self.assertNotEqual(result.returncode, 0)
        result, _ = self.run_idc_settings(IDC_REGISTRY="https://registry.example")
        self.assertNotEqual(result.returncode, 0)

    def test_client_arguments_with_and_without_features(self):
        workflow = (ROOT / ".github/workflows/release-binaries.yml").read_text()
        step = workflow.split("      - name: Build client candidates\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        script = script.replace("${{ matrix.target }}", "test-target")
        # POSIX positional parameters also work on macOS's Bash 3.2.
        # Run that portion under sh as well as bash to guard portability.
        for shell in ("bash", "sh"):
            for features in ("", "astra-cli/release-vendored-openssl"):
                with self.subTest(shell=shell, features=features):
                    body = script if shell == "bash" else script.replace("set -euo pipefail", "set -eu")
                    result = subprocess.run(
                        [shell, "-c", 'cargo() { printf "%s\\n" "$@"; };\n' + body],
                        env={**os.environ, "RELEASE_FEATURES": features},
                        capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    expected = ["build", "--release", "--locked", "--no-default-features"]
                    if features:
                        expected += ["--features", features]
                    expected += ["--manifest-path", "Cargo.toml", "--target", "test-target",
                                 "-p", "astra-cli", "--bin", "astra",
                                 "-p", "astra-edge", "--bin", "astra-edge"]
                    self.assertEqual(result.stdout.splitlines(), expected)

    def test_docker_optional_mirrors_unset_and_empty(self):
        dockerfile = (ROOT / "Dockerfile").read_text().replace("\\\n", "")
        commands = re.findall(r"^RUN (set -eux;.*)$", dockerfile, re.MULTILINE)
        commands = [command for command in commands
                    if "CARGO_REGISTRY" in command or "DEBIAN_MIRROR" in command]
        self.assertEqual(len(commands), 3)
        stubs = '\n'.join(f'{name}() {{ :; }}' for name in
                          ("apt_get", "rm", "groupadd", "useradd"))
        for empty in (False, True):
            env = {key: value for key, value in os.environ.items()
                   if key not in ("CARGO_REGISTRY", "DEBIAN_MIRROR")}
            if empty:
                env.update(CARGO_REGISTRY="", DEBIAN_MIRROR="")
            for command in commands:
                with self.subTest(empty=empty, command=command[:70]):
                    result = subprocess.run(
                        ["sh", "-c", stubs + '\n' + command.replace("apt-get", "apt_get")], env=env,
                        capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
