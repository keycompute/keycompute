#!/usr/bin/env python3
"""Regression tests for local Web probes (Python stdlib + Docker Compose).

Run: python3 scripts/tests/web_healthcheck_test.py
WEB_HEALTHCHECK_TEST_IMAGE may name an existing production Web image for an
additional runtime check. By default, use Dockerfile.web's Nginx runtime base.
Only a uniquely named, network-isolated fixture is started; no application
containers, published ports, database volumes, or real proxy are used.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
PROBE = ["wget", "-Y", "off", "-q", "-O", "/dev/null", "http://127.0.0.1/"]
DOCKERFILE = (ROOT / "Dockerfile.web").read_text()
COMPOSE_LAYOUTS = (
    ("docker-compose.yml",),
    ("docker-compose.yml", "docker-compose.proxy.yml"),
    ("docker-compose.yml", "docker-compose.dev.yml"),
    ("docker-compose.replicas.yml",),
    ("docker-compose.replicas.yml", "docker-compose.proxy.yml"),
)


def run(*args, timeout=30, check=True):
    result = subprocess.run(
        args, cwd=ROOT, text=True, capture_output=True, timeout=timeout
    )
    if check and result.returncode:
        raise RuntimeError(f"Command {args!r} failed: {result.stderr}")
    return result


class ProbeConfigurationTests(unittest.TestCase):
    def test_compose_layouts_share_direct_exec_probe(self):
        for files in COMPOSE_LAYOUTS:
            with self.subTest(files=files):
                args = [
                    "docker", "compose", "--project-name", "kc-healthcheck-test",
                    "--env-file", str(ROOT / ".env.example"),
                ]
                for filename in files:
                    args.extend(("-f", str(ROOT / filename)))
                # Inspect only the probe; never emit the resolved environment.
                config = json.loads(run(*args, "config", "--format", "json").stdout)
                health = config["services"]["keycompute-web"]["healthcheck"]
                self.assertEqual(health["test"], ["CMD", *PROBE])
                self.assertEqual(health["interval"], "30s")
                self.assertEqual(health["timeout"], "5s")
                self.assertEqual(health["start_period"], "10s")
                self.assertEqual(health["retries"], 3)
                self.assertFalse(health.get("disable", False))

    def test_dockerfile_probe_matches_compose(self):
        logical_lines = re.sub(r"\\\n\s*", " ", DOCKERFILE)
        matches = re.findall(
            r"^HEALTHCHECK\s+(.+?)\s+CMD\s+(\[.*\])\s*$",
            logical_lines, re.MULTILINE,
        )
        self.assertEqual(len(matches), 1, "Expect one exec-form HEALTHCHECK")
        options, command = matches[0]
        self.assertEqual(json.loads(command), PROBE)
        self.assertEqual(
            set(options.split()),
            {"--interval=30s", "--timeout=5s", "--start-period=10s", "--retries=3"},
        )


class ProbeRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        runtime = re.findall(r"^FROM\s+(\S+)\s+AS\s+runtime\s*$", DOCKERFILE, re.M)
        if len(runtime) != 1:
            raise RuntimeError("Cannot identify Dockerfile.web runtime image")
        image = os.environ.get("WEB_HEALTHCHECK_TEST_IMAGE", runtime[0])
        cls.fixture = tempfile.TemporaryDirectory(prefix="kc-web-healthcheck-")
        cls.addClassCleanup(cls.fixture.cleanup)
        conf = Path(cls.fixture.name) / "nginx.conf"
        conf.write_text('''
worker_processes 1;
error_log /dev/stderr warn;
pid /tmp/nginx.pid;
events { worker_connections 32; }
http {
    access_log off;
    client_body_temp_path /tmp/client_temp;
    proxy_temp_path /tmp/proxy_temp;
    fastcgi_temp_path /tmp/fastcgi_temp;
    uwsgi_temp_path /tmp/uwsgi_temp;
    scgi_temp_path /tmp/scgi_temp;
    server {
        listen 127.0.0.1:80;
        location = / { default_type text/html; return 200 "<html>ok</html>"; }
        location = /missing { return 404; }
        location = /unavailable { return 503; }
    }
}
''')
        cls.container = "kc-web-healthcheck-" + uuid.uuid4().hex[:12]
        # Register before creation, so setup failures also clean up our fixture.
        cls.addClassCleanup(
            lambda: run("docker", "rm", "-f", cls.container, check=False)
        )
        run(
            "docker", "run", "--detach", "--name", cls.container,
            "--network", "none", "--no-healthcheck", "--read-only",
            "--tmpfs", "/tmp:rw,nosuid,nodev,size=16m",
            "--memory", "64m", "--pids-limit", "64",
            "--mount", f"type=bind,source={cls.fixture.name},target=/test,readonly",
            "--entrypoint", "nginx", image,
            "-c", "/test/nginx.conf", "-g", "daemon off;", timeout=180,
        )
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if cls.probe().returncode == 0:
                return
            time.sleep(0.2)
        logs = run("docker", "logs", cls.container, check=False)
        raise RuntimeError("Nginx fixture did not become ready: " + logs.stderr)

    @classmethod
    def probe(cls, *, proxy="", no_proxy="", url=None, command=None):
        args = ["docker", "exec"]
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
                    "http_proxy", "https_proxy", "all_proxy"):
            args.extend(("-e", f"{key}={proxy}"))
        for key in ("NO_PROXY", "no_proxy"):
            args.extend(("-e", f"{key}={no_proxy}"))
        actual = list(PROBE if command is None else command)
        if url is not None:
            actual[-1] = url
        return run(*args, cls.container, *actual, timeout=10, check=False)

    def test_success_without_proxy(self):
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "", "Do not fill health logs with HTML")

    def test_success_with_unreachable_proxy(self):
        for exclusions in ("", "localhost,127.0.0.1", "*"):
            with self.subTest(no_proxy=exclusions):
                result = self.probe(proxy="http://127.0.0.1:9", no_proxy=exclusions)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_negative_control_original_probe_uses_broken_proxy(self):
        result = self.probe(
            proxy="http://127.0.0.1:9",
            command=["wget", "-qO-", "http://127.0.0.1/"],
        )
        self.assertNotEqual(result.returncode, 0, "Broken proxy must be effective")

    def test_http_errors_remain_failures(self):
        for path, status in (("missing", "404"), ("unavailable", "503")):
            with self.subTest(status=status):
                result = self.probe(
                    proxy="http://127.0.0.1:9", url=f"http://127.0.0.1/{path}"
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(status, result.stderr)

    def test_closed_port_remains_failure(self):
        result = self.probe(url="http://127.0.0.1:9/")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(result.stderr, "Keep connection failure diagnostics")


if __name__ == "__main__":
    unittest.main(verbosity=2)
