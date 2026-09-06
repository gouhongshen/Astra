#!/usr/bin/env python3
"""Execute release shell entrypoints with build/network commands stubbed out."""

import os
import json
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseShellTests(unittest.TestCase):
    def run_idc_settings(self, **overrides):
        workflow = (ROOT / ".github/workflows/build_push_to_idc.yml").read_text()
        script = workflow.split("        run: |\n", 1)[1].split("\n  candidates:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        env = {
            **os.environ,
            "IDC_REGISTRY": "registry.example:5000",
            "IDC_IMAGE": "registry.example:5000/team/astra",
            "IDC_RUNNER": "idc-amd64",
            "IDC_USERNAME": "test-user",
            "IDC_PASSWORD": "test-password",
            "GITHUB_SHA": "a" * 40,
            "GITHUB_RUN_ID": "123",
            "GITHUB_OUTPUT": "/dev/stdout",
            **overrides,
        }
        return subprocess.run(["bash", "-c", script], env=env,
                              capture_output=True, text=True)

    def test_idc_build_identity_and_runner(self):
        result = self.run_idc_settings()
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(outputs["source_sha"], "a" * 40)
        self.assertRegex(outputs["image_version"], r"^idc-\d{8}T\d{6}Z-" + "a" * 40 + r"-123-amd64$")
        self.assertEqual(json.loads(outputs["matrix"]), {"include": [
            {"platform": "linux/amd64", "runner": "idc-amd64", "slug": "linux-amd64"}]})
        self.assertNotIn("test-password", result.stdout + result.stderr)

    def test_idc_missing_configuration_stops_before_build(self):
        for key in ("IDC_REGISTRY", "IDC_IMAGE", "IDC_RUNNER", "IDC_USERNAME", "IDC_PASSWORD"):
            with self.subTest(key=key):
                result = self.run_idc_settings(**{key: ""})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Missing required IDC configuration: " + key, result.stdout)
                self.assertNotIn("matrix=", result.stdout)

    def test_idc_rejects_wrong_or_tagged_repository(self):
        for image in ("docker.io/team/astra", "registry.example:5000/",
                      "registry.example:5000/team/astra:latest",
                      "registry.example:5000/team/astra@sha256:abc",
                      "registry.example:5000/team/astra bad"):
            with self.subTest(image=image):
                self.assertNotEqual(self.run_idc_settings(IDC_IMAGE=image).returncode, 0)
        self.assertNotEqual(self.run_idc_settings(IDC_REGISTRY="https://registry.example").returncode, 0)

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
