#!/usr/bin/env python3
"""Exercise the production proxy config in an isolated, network-disabled container."""
import pathlib
import subprocess
import sys
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[2]
IMAGE = sys.argv[1] if len(sys.argv) > 1 else "keycompute-web:latest"
NAME = "kc-capability-log-test-" + uuid.uuid4().hex[:12]
TOKEN = "secret_canary_" + uuid.uuid4().hex

def docker(*args, check=True):
    return subprocess.run(["docker", *args], text=True, capture_output=True, check=check)

try:
    docker("run", "--detach", "--name", NAME, "--network", "none",
           "--add-host", "keycompute-server:127.0.0.1", "--entrypoint", "nginx",
           "--mount", f"type=bind,src={ROOT / 'nginx/nginx.conf'},dst=/etc/nginx/nginx.conf,readonly",
           IMAGE, "-g", "daemon off;")
    for _ in range(30):
        ready = docker("exec", NAME, "nginx", "-t", check=False)
        if ready.returncode == 0:
            break
        time.sleep(0.1)
    else:
        raise AssertionError("isolated nginx did not start")
    for path in (f"/api/v1/invitations/{TOKEN}/accept",
                 f"/api/v1/auth/verify-reset-token/{TOKEN}"):
        result = docker("exec", NAME, "wget", "-S", "-O-", "--timeout=3",
                        f"--header=Referer: http://example.invalid/{TOKEN}",
                        f"http://127.0.0.1{path}?private={TOKEN}", check=False)
        assert "502" in result.stderr, result.stderr
    # Stop before inspecting logs so all access log buffers have been flushed.
    docker("stop", "--time", "2", NAME)
    result = docker("logs", NAME)
    logs = result.stdout + result.stderr
    assert TOKEN not in logs, "capability leaked to proxy access/error logs"
    assert "/api/v1/invitations/[redacted]/accept" in logs, logs
    assert "/api/v1/auth/verify-reset-token/[redacted]" in logs, logs
    print("PASS: production proxy logs preserve status without capability/query/referrer secrets")
finally:
    docker("rm", "--force", NAME, check=False)
