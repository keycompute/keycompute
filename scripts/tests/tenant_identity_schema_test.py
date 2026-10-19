#!/usr/bin/env python3
"""Exercise the real tenant schema in a newly created, disposable PostgreSQL DB.

Requires an explicitly labelled task-owned PostgreSQL container. It never reads
application credentials or discovers a production database. No trigger is disabled.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import traceback
import uuid

ROOT = Path(__file__).resolve().parents[2]


class Database:
    def __init__(self, container: str) -> None:
        if os.environ.get("KC_TENANT_TEST_ACK_ISOLATED") != "1":
            raise RuntimeError("explicit isolated-test acknowledgement is required")
        if not container.startswith("kc-tenant-test-db-"):
            raise RuntimeError("container is not in the dedicated tenant test namespace")
        label = subprocess.check_output(
            ["docker", "inspect", "--format", '{{index .Config.Labels "task"}}', container],
            text=True,
        ).strip()
        if label != "kc-tenant-system":
            raise RuntimeError("container is not labelled for this isolated test task")
        env = subprocess.check_output(
            ["docker", "inspect", "--format", "{{range .Config.Env}}{{println .}}{{end}}", container],
            text=True,
        )
        role = next(
            (line.partition("=")[2] for line in env.splitlines() if line.startswith("POSTGRES_USER=")),
            "",
        )
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", role):
            raise RuntimeError("isolated PostgreSQL container has no safe POSTGRES_USER")
        self.container = container
        self.role = role
        self.name = "kc_tenant_security_" + uuid.uuid4().hex
        self.command = [
            "docker", "exec", "-i", container, "psql", "-X", "-A", "-t",
            "-v", "ON_ERROR_STOP=1", "-U", self.role, "-d", self.name,
        ]

    def create(self) -> None:
        subprocess.run(
            ["docker", "exec", self.container, "createdb", "-U", self.role, self.name],
            check=True, capture_output=True, text=True,
        )
        schema = (ROOT / "crates/keycompute-db/migrations/001_init.sql").read_text()
        self.sql("BEGIN;\n" + schema + "\nCOMMIT;")

    def sql(self, sql: str, success: bool = True) -> str:
        result = subprocess.run(
            self.command, input=sql, text=True, capture_output=True, timeout=20,
        )
        if success and result.returncode:
            raise AssertionError(result.stderr[-6000:])
        if not success and not result.returncode:
            raise AssertionError("invalid transaction was unexpectedly accepted")
        return result.stdout.strip()

    def reject(self, sql: str, expected: str) -> None:
        result = subprocess.run(
            self.command, input=sql, text=True, capture_output=True, timeout=20,
        )
        if result.returncode == 0 or expected.lower() not in result.stderr.lower():
            raise AssertionError("unexpected rejection outcome:\n" + result.stderr[-4000:])

    def start(self, sql: str) -> subprocess.Popen[str]:
        process = subprocess.Popen(
            self.command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True,
        )
        assert process.stdin is not None
        process.stdin.write(sql)
        process.stdin.close()
        process.stdin = None
        return process

    def close(self) -> None:
        subprocess.run(
            ["docker", "exec", self.container, "dropdb", "--if-exists", "--force",
             "-U", self.role, self.name],
            check=True, capture_output=True, text=True,
        )


def uid() -> str:
    return str(uuid.uuid4())


def run(db: Database) -> None:
    assert db.sql("SELECT COUNT(*) FROM users;") == "0", "schema must not create a fictitious root"
    root, owner, peer, other_admin, mover = uid(), uid(), uid(), uid(), uid()
    tenant, other, empty = uid(), uid(), uid()
    db.sql(f"INSERT INTO users(id,email,platform_role) VALUES('{root}','root@fixture.invalid','root');")
    root2 = uid()
    db.reject(
        f"INSERT INTO users(id,email,platform_role) VALUES('{root2}','root2@fixture.invalid','root');",
        "uq_users_single_root",
    )
    db.reject(f"DELETE FROM users WHERE id='{root}';", "root")
    db.reject("TRUNCATE users CASCADE;", "root")
    assert db.sql(f"SELECT COUNT(*) FROM users WHERE id='{root}' AND platform_role='root';") == "1"
    print("PASS: bootstrap root is unique and cannot be deleted or truncated")
    db.sql(f"INSERT INTO tenants(id,name,slug) VALUES('{empty}','Empty','empty-fixture');")
    assert db.sql("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='tenants' AND column_name='owner_user_id';") == "0"
    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE tenant_id='{empty}';") == "0"
    print("PASS: a platform-provisioned tenant may begin without a membership and has no owner column")
    db.sql(f"""BEGIN;
        INSERT INTO users(id,email,platform_role) VALUES
          ('{owner}','admin-a@fixture.invalid','none'),
          ('{peer}','admin-peer@fixture.invalid','none'),
          ('{other_admin}','admin-b@fixture.invalid','none'),
          ('{mover}','mover@fixture.invalid','none');
        INSERT INTO tenants(id,name,slug) VALUES
          ('{tenant}','Tenant A','fixture-a'),
          ('{other}','Tenant B','fixture-b');
        INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role) VALUES
          ('{tenant}','{owner}','admin'),('{tenant}','{peer}','admin'),
          ('{other}','{other_admin}','admin'),('{tenant}','{mover}','member');
        COMMIT;""")
    db.reject(
        f"INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role) VALUES('{other}','{mover}','member');",
        "uq_tenant_memberships_one_current_tenant_per_user",
    )
    db.sql(f"""UPDATE tenant_memberships SET status='removed' WHERE tenant_id='{tenant}' AND user_id='{mover}';
        INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role) VALUES('{other}','{mover}','member');""")
    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE user_id='{mover}' AND status<>'removed';") == "1"
    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE user_id='{mover}';") == "2"
    assert db.sql("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='users' AND column_name IN ('role','tenant_id');") == "0"
    print("PASS: one current tenant per user with retained removed history")

    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE tenant_id='{tenant}' AND tenant_role='admin' AND status='active';") == "2"
    db.sql(f"UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id='{tenant}' AND user_id='{owner}';")
    assert db.sql(f"SELECT authz_version FROM tenant_memberships WHERE tenant_id='{tenant}' AND user_id='{owner}';") == "2"
    db.sql(f"UPDATE users SET status='suspended' WHERE id='{owner}';")
    assert db.sql(f"SELECT status FROM users WHERE id='{owner}';") == "suspended"
    db.sql(f"UPDATE users SET status='active' WHERE id='{owner}';")
    db.reject(f"BEGIN; UPDATE users SET platform_role='none' WHERE id='{root}'; COMMIT;", "root")
    print("PASS: multiple tenant administrators are peers and root remains immutable")

    unknown = root  # Existing platform root is deliberately not a tenant member.
    db.reject(
        f"INSERT INTO produce_ai_keys(tenant_id,user_id,name,produce_ai_key_hash,produce_ai_key_preview) VALUES('{tenant}','{unknown}','bad','{uuid.uuid4().hex}','fixture');",
        "foreign key",
    )
    db.sql(f"""INSERT INTO user_balances(tenant_id,user_id,available_balance)
        VALUES('{tenant}','{mover}',12),('{other}','{mover}',34);""")
    assert db.sql(f"SELECT available_balance::int FROM user_balances WHERE tenant_id='{tenant}' AND user_id='{mover}';") == "12"
    assert db.sql(f"SELECT available_balance::int FROM user_balances WHERE tenant_id='{other}' AND user_id='{mover}';") == "34"
    key = uid()
    db.sql(f"""INSERT INTO produce_ai_keys(id,tenant_id,user_id,name,produce_ai_key_hash,produce_ai_key_preview)
        VALUES('{key}','{tenant}','{owner}','old-key','{uuid.uuid4().hex}','fixture');
        UPDATE tenant_memberships SET status='removed' WHERE tenant_id='{tenant}' AND user_id='{owner}';""")
    assert db.sql(f"SELECT revoked FROM produce_ai_keys WHERE id='{key}';") == "t", "revoking membership must revoke previously issued inference keys"
    assert db.sql(f"SELECT COUNT(*) FROM user_balances WHERE tenant_id='{tenant}' AND user_id='{mover}';") == "1"
    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE tenant_id='{tenant}' AND user_id='{owner}' AND status='removed' AND removed_at IS NOT NULL;") == "1"
    db.sql(f"""UPDATE tenant_memberships
        SET tenant_role='admin',status='active',invited_by='{root}',joined_at=clock_timestamp(),removed_at=NULL
        WHERE tenant_id='{tenant}' AND user_id='{owner}' AND status='removed';""")
    assert db.sql(f"SELECT COUNT(*) FROM tenant_memberships WHERE tenant_id='{tenant}' AND user_id='{owner}' AND status='active' AND tenant_role='admin';") == "1"
    db.sql(f"UPDATE tenant_memberships SET status='removed' WHERE tenant_id='{tenant}' AND user_id='{owner}';")
    print("PASS: tenant wallets, key revocation and root/operator direct administrator regrant")

    event = uid()
    db.sql(f"""INSERT INTO tenant_audit_events(id,scope_type,tenant_id,actor_user_id,platform_role,action,resource_type,resource_id)
        VALUES('{event}','tenant','{tenant}','{peer}','none','fixture.read','response','resp_fixture');""")
    db.reject(f"DELETE FROM tenant_audit_events WHERE id='{event}';", "immutable")
    db.reject(f"UPDATE tenant_audit_events SET action='changed' WHERE id='{event}';", "immutable")
    assert db.sql(f"SELECT resource_id FROM tenant_audit_events WHERE id='{event}';") == "resp_fixture"
    print("PASS: append-only text-ID audit records")

    # Simulate request code that already owns the tenant row. A membership
    # administrative write must not create a reverse wait through a global lock.
    p1 = db.start(f"BEGIN; SET LOCAL statement_timeout='6s'; SELECT id FROM tenants WHERE id='{other}' FOR UPDATE; SELECT pg_sleep(0.7); UPDATE tenants SET responses_idempotency_claim_count=responses_idempotency_claim_count+1 WHERE id='{other}'; COMMIT;")
    time.sleep(0.15)
    p2 = db.start(f"BEGIN; SET LOCAL statement_timeout='6s'; UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id='{other}' AND user_id='{other_admin}'; COMMIT;")
    for p in (p1, p2):
        out, err = p.communicate(timeout=10)
        if p.returncode:
            raise AssertionError("request/admin lock order regressed:\n" + err + out)
    print("PASS: request counter and membership administration have no lock inversion")

    # Root is created only once during bootstrap. Neither a second insert nor
    # any later role transition may create another root identity.
    db.reject(
        f"BEGIN; UPDATE users SET platform_role='root' WHERE id='{peer}'; COMMIT;",
        "uq_users_single_root",
    )
    assert db.sql("SELECT COUNT(*) FROM users WHERE platform_role='root' AND status='active';") == "1"
    print("PASS: an existing user cannot be promoted to a second root")

    actor, empty_tenant, historical_event = uid(), uid(), uid()
    db.sql(f"""BEGIN;
        INSERT INTO users(id,email) VALUES('{actor}','deleted-actor@fixture.invalid');
        INSERT INTO tenants(id,name,slug) VALUES('{empty_tenant}','Disposable','disposable-fixture');
        INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role) VALUES('{empty_tenant}','{actor}','admin');
        INSERT INTO tenant_audit_events(id,scope_type,tenant_id,actor_user_id,platform_role,action,resource_type,resource_id)
          VALUES('{historical_event}','tenant','{empty_tenant}','{actor}','none','fixture.create','tenant','{empty_tenant}');
        COMMIT;
        DELETE FROM tenants WHERE id='{empty_tenant}';
        DELETE FROM users WHERE id='{actor}';""")
    assert db.sql(f"SELECT COUNT(*) FROM tenant_audit_events WHERE id='{historical_event}' AND tenant_id='{empty_tenant}' AND actor_user_id='{actor}';") == "1"
    print("PASS: tenant and actor deletion preserve historical audit identity")

    before = db.sql("SELECT COUNT(*) FROM tenant_audit_events;")
    schema = (ROOT / "crates/keycompute-db/migrations/001_init.sql").read_text()
    db.sql("BEGIN;\n" + schema + "\nCOMMIT;")
    assert db.sql("SELECT COUNT(*) FROM tenant_audit_events;") == before
    assert db.sql("SELECT COUNT(*) FROM users WHERE platform_role='root' AND status='active';") == "1"
    print("PASS: final baseline replay preserves identities and audit records")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    args = parser.parse_args()
    db = Database(args.container)
    try:
        db.create()
        run(db)
        print("All tenant schema checks passed.")
        return 0
    finally:
        db.close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError, subprocess.SubprocessError) as exc:
        traceback.print_exc()
        sys.exit(1)
