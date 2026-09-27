#!/usr/bin/env python3
"""Build and exercise both real Compose MinIO definitions in disposable projects."""

import json
from pathlib import Path
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]


def run(*args, capture=False, timeout=900):
    return subprocess.run(
        args, cwd=ROOT, check=True, text=True,
        stdout=subprocess.PIPE if capture else None, timeout=timeout
    ).stdout


def smoke(compose_file):
    config = json.loads(run("docker", "compose", "-f", compose_file,
                            "config", "--format", "json", capture=True))
    services = {name: config["services"][name] for name in ("minio", "minio-init")}
    # Retain commands, credentials, healthcheck, init and dependency conditions.
    # Only isolate host resources; never touch the caller's dev stack or volumes.
    for service in services.values():
        for key in ("container_name", "ports", "networks"):
            service.pop(key, None)
    services["minio"]["volumes"] = [{"type": "volume", "source": "data", "target": "/data"}]
    project = "buzz-minio-smoke-" + uuid.uuid4().hex[:12]
    with tempfile.TemporaryDirectory(prefix=project) as directory:
        path = Path(directory) / "compose.json"
        path.write_text(json.dumps({"services": services, "volumes": {"data": {}}}))
        command = ("docker", "compose", "-p", project, "-f", str(path))
        try:
            run(*command, "build", "minio", "minio-init", timeout=1800)
            run(*command, "up", "-d", "--wait", "--wait-timeout", "120", "minio")
            # The real entrypoint must succeed twice (bucket creation is idempotent).
            for _ in range(2):
                run(*command, "run", "--rm", "--no-deps", "minio-init")
            run(*command, "run", "--rm", "--no-deps", "--entrypoint", "/bin/sh",
                "minio-init", "-ec", """
mc alias set local http://minio:9000 buzz_dev buzz_dev_secret
mc stat local/buzz-media
printf 'buzz minio smoke\n' > /tmp/payload
mc cp /tmp/payload local/buzz-media/smoke.txt
mc cat local/buzz-media/smoke.txt > /tmp/received
cmp /tmp/payload /tmp/received
mc rm local/buzz-media/smoke.txt
if mc stat local/buzz-media/smoke.txt; then
    printf 'Deleted object remains readable\n' >&2
    exit 1
fi
""")
            # Check the init's private-bucket policy over HTTP, not source text.
            status = run(*command, "exec", "-T", "minio", "curl", "-sS", "-o",
                         "/dev/null", "-w", "%{http_code}",
                         "http://localhost:9000/buzz-media/", capture=True).strip()
            if status != "403":
                raise RuntimeError(f"Anonymous bucket listing returned {status}, expected 403")
            print(f"PASS {compose_file}: health, init twice, S3 put/get/delete, anonymous denied",
                  flush=True)
        finally:
            run(*command, "down", "--volumes", "--remove-orphans", timeout=120)


if __name__ == "__main__":
    for compose in ("docker-compose.yml", "docker-compose.harness.yml"):
        smoke(compose)
