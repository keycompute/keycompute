#!/usr/bin/env python3
"""Rehearse a full synthetic tenant snapshot restore in labelled test PostgreSQL.

Never accepts a DSN, database name or existing dump. Both source and destination
are newly created fixtures; the archive is private and removed on completion.
This validates the restore mechanism, not a production backup or release approval.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from tenant_identity_schema_test import Database, ROOT, run as exercise_schema


def preflight(container: str) -> None:
    if os.environ.get('KC_TENANT_TEST_ACK_ISOLATED') != '1':
        raise RuntimeError('explicit isolated-test acknowledgement is required')
    if not re.fullmatch(r'kc-tenant-test-db-[A-Za-z0-9_-]+', container):
        raise RuntimeError('only a labelled disposable tenant test container is accepted')


def constraint_fingerprint(db: Database) -> str:
    # PostgreSQL's first dump/restore can rewrite equivalent varchar-array
    # casts to per-element text casts. Reparse CHECK definitions using the
    # same PostgreSQL parser on EMPTY TEMPORARY tables rather than stripping
    # expressions or weakening the comparison. No persistent table is altered.
    return db.sql("""BEGIN;
CREATE TEMP TABLE kc_restore_constraint_signatures (
 table_name text, constraint_name text, kind text, valid boolean,
 is_deferrable boolean, is_deferred boolean, definition text
) ON COMMIT DROP;
DO $canonical$ DECLARE tab record; con record; canonical text;
BEGIN
 FOR tab IN SELECT oid,relname FROM pg_class WHERE relnamespace='public'::regnamespace AND relkind='r' ORDER BY relname LOOP
  EXECUTE format('CREATE TEMP TABLE kc_restore_check_parse (LIKE %s) ON COMMIT DROP',tab.oid::regclass);
  FOR con IN SELECT oid,conname,contype,convalidated,condeferrable,condeferred FROM pg_constraint WHERE conrelid=tab.oid ORDER BY conname LOOP
   canonical := pg_get_constraintdef(con.oid);
   IF con.contype='c' THEN
    EXECUTE format('ALTER TABLE pg_temp.kc_restore_check_parse ADD CONSTRAINT %I %s',con.conname,canonical);
    SELECT pg_get_constraintdef(oid) INTO canonical FROM pg_constraint WHERE conrelid='pg_temp.kc_restore_check_parse'::regclass AND conname=con.conname;
   END IF;
   INSERT INTO kc_restore_constraint_signatures VALUES(tab.relname,con.conname,con.contype,con.convalidated,con.condeferrable,con.condeferred,canonical);
  END LOOP;
  DROP TABLE pg_temp.kc_restore_check_parse;
 END LOOP;
END $canonical$;
SELECT table_name,constraint_name,kind,valid,is_deferrable,is_deferred,definition FROM kc_restore_constraint_signatures ORDER BY 1,2;
ROLLBACK;""")


def fingerprints(db: Database) -> dict[str, str]:
    names = db.sql("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename;").splitlines()
    if not names or any(not re.fullmatch(r'[a-z_][a-z0-9_]*', name) for name in names):
        raise RuntimeError('unexpected fixture table identifiers')
    statements = [
        f"SELECT '{name}' AS name, COUNT(*)::text AS rows, "
        f"md5(COALESCE(jsonb_agg(to_jsonb(row) ORDER BY to_jsonb(row)::text)::text,'[]')) AS digest "
        f'FROM public."{name}" row'
        for name in names
    ]
    rows = db.sql(' UNION ALL '.join(statements) + ' ORDER BY name;')
    constraints = constraint_fingerprint(db)
    triggers = db.sql("SELECT tgrelid::regclass::text,tgname,tgenabled,pg_get_triggerdef(oid) FROM pg_trigger WHERE NOT tgisinternal AND tgrelid IN (SELECT oid FROM pg_class WHERE relnamespace='public'::regnamespace) ORDER BY 1,2;")
    return {'tables': str(len(names)), 'rows': rows, 'constraints': constraints, 'triggers': triggers}


def check_invariants(db: Database) -> None:
    assert db.sql("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='users' AND column_name IN ('tenant_id','role');") == '0'
    assert db.sql("SELECT COUNT(*) FROM tenants t LEFT JOIN tenant_memberships m ON m.tenant_id=t.id AND m.user_id=t.owner_user_id LEFT JOIN users u ON u.id=t.owner_user_id WHERE m.user_id IS NULL OR m.tenant_role<>'admin' OR m.status<>'active' OR u.status<>'active';") == '0'
    assert db.sql("SELECT COUNT(*) FROM produce_ai_keys k JOIN tenant_memberships m ON m.tenant_id=k.tenant_id AND m.user_id=k.user_id WHERE m.status='removed' AND NOT k.revoked;") == '0'
    root = db.sql("SELECT id FROM users WHERE platform_role='root' AND status='active';")
    if not re.fullmatch(r'[0-9a-f-]{36}', root):
        raise RuntimeError('fixture must have exactly one active root after schema exercises')
    db.reject(f"DELETE FROM users WHERE id='{root}';", 'root')
    db.reject("UPDATE tenant_audit_events SET action='restore-mutated';", 'immutable')
    owner = db.sql("SELECT owner_user_id FROM tenants ORDER BY id LIMIT 1;")
    db.reject(f"UPDATE users SET status='suspended' WHERE id='{owner}';", 'owner')


def run(container: str) -> dict:
    preflight(container)
    # Database independently verifies the container task label and safe DB role.
    source, restored = Database(container), Database(container)
    source_created, restored_created = False, False
    try:
        subprocess.run(['docker','exec',container,'createdb','-U',source.role,source.name],
            check=True, capture_output=True, text=True, timeout=30)
        source_created = True
        source.sql('BEGIN;\n'+(ROOT/'crates/keycompute-db/migrations/001_init.sql').read_text()+'\nCOMMIT;')
        exercise_schema(source)
        # A pending, one-time hashed invitation is also retained by the snapshot.
        source.sql("INSERT INTO tenant_invitations(tenant_id,invited_by,email,tenant_role,token_hash,expires_at) SELECT id,owner_user_id,'restore-invite@fixture.invalid','member',repeat('a',64),clock_timestamp()+interval '1 hour' FROM tenants ORDER BY id LIMIT 1;")
        before = fingerprints(source)
        with tempfile.TemporaryDirectory(prefix='kc-tenant-restore-') as temp:
            archive = Path(temp)/'synthetic.dump'
            with archive.open('xb') as output:
                os.chmod(archive, 0o600)
                subprocess.run(['docker','exec',container,'pg_dump','-U',source.role,
                    '--format=custom','--no-owner','--no-privileges','--dbname',source.name],
                    check=True, stdout=output, stderr=subprocess.PIPE, timeout=90)
            archive_hash = hashlib.sha256(archive.read_bytes()).hexdigest()
            archive_size = archive.stat().st_size
            subprocess.run(['docker','exec',container,'createdb','-U',restored.role,restored.name],
                check=True, capture_output=True, text=True, timeout=30)
            restored_created = True
            with archive.open('rb') as data:
                subprocess.run(['docker','exec','-i',container,'pg_restore','-U',restored.role,
                    '--single-transaction','--exit-on-error','--no-owner','--no-privileges',
                    '--dbname',restored.name], stdin=data, check=True,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90)
            after = fingerprints(restored)
            if before != after:
                different = [name for name in before if before[name] != after.get(name)]
                print('Different synthetic fingerprint sections: ' + ','.join(different), file=sys.stderr)
                raise RuntimeError('restored row, constraint or trigger fingerprints differ')
            check_invariants(restored)
            # Replay final startup schema; all row and constraint identities must remain.
            restored.sql('BEGIN;\n'+(ROOT/'crates/keycompute-db/migrations/001_init.sql').read_text()+'\nCOMMIT;')
            if fingerprints(restored) != after:
                raise RuntimeError('schema replay changed restored fixture state')
            return {'passed': True, 'tables_verified': int(after['tables']),
                    'synthetic_archive_bytes': archive_size, 'synthetic_archive_sha256': archive_hash,
                    'row_constraint_trigger_fingerprints_equal': True,
                    'restored_authority_constraints_enforced': True, 'startup_replay_unchanged': True,
                    'production_backup_or_release_verified': False}
    finally:
        errors = []
        for db, created in ((restored, restored_created), (source, source_created)):
            if created:
                try:
                    db.close()
                except (OSError, subprocess.SubprocessError) as error:
                    errors.append(error)
        if errors:
            raise RuntimeError('disposable restore fixture cleanup failed') from errors[0]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--container', required=True)
    args = parser.parse_args()
    try:
        result = run(args.container)
    except (AssertionError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        print('Failure category: ' + type(error).__name__, file=sys.stderr)
        print(json.dumps({'passed': False, 'message': 'isolated restore rehearsal failed; no production operation was requested'}))
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())
