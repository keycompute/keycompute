"""Safety and failure cleanup for synthetic tenant restore rehearsal."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import MagicMock, patch

# Kept next to the CI guard tests after integration; no production connection.
ROOT = Path(__file__).resolve().parents[2]
TESTS = ROOT/'scripts/tests'
sys.path.insert(0, str(TESTS))
MODULE_PATH = TESTS/'tenant_restore_rehearsal.py'
spec = importlib.util.spec_from_file_location('tenant_restore_rehearsal', MODULE_PATH)
restore = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restore)


class RestoreSafetyTests(unittest.TestCase):
    def fixtures(self):
        source = MagicMock(name='synthetic_source')
        source.name, source.role = 'kc_tenant_security_source_fixture', 'fixture'
        target = MagicMock(name='synthetic_target')
        target.name, target.role = 'kc_tenant_security_target_fixture', 'fixture'
        return source, target

    def test_production_container_or_missing_ack_rejected_before_inspection(self):
        with patch.object(restore, 'Database') as database:
            for name in ['keycompute-postgres', 'postgres', '', 'kc-tenant-test-db-', 'kc-tenant-test-db-a;danger']:
                with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}):
                    with self.assertRaises(RuntimeError):
                        restore.run(name)
            with patch.dict(os.environ, {}, clear=True):
                with self.assertRaises(RuntimeError):
                    restore.run('kc-tenant-test-db-fixture')
            database.assert_not_called()

    def test_source_schema_failure_drops_only_successfully_created_fixture(self):
        source, target = self.fixtures()
        source.sql.side_effect = AssertionError('synthetic schema fault')
        with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}), \
             patch.object(restore, 'Database', side_effect=[source, target]), \
             patch.object(restore.subprocess, 'run'):
            with self.assertRaises(AssertionError):
                restore.run('kc-tenant-test-db-fixture')
        source.close.assert_called_once()
        target.close.assert_not_called()

    def test_creation_failure_does_not_drop_a_database_we_did_not_create(self):
        source, target = self.fixtures()
        with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}), \
             patch.object(restore, 'Database', side_effect=[source, target]), \
             patch.object(restore.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, ['createdb'])):
            with self.assertRaises(subprocess.CalledProcessError):
                restore.run('kc-tenant-test-db-fixture')
        source.close.assert_not_called()
        target.close.assert_not_called()

    def test_restore_failure_cleans_both_owned_databases_and_private_archive(self):
        source, target = self.fixtures()
        archives = []
        def command(args, **kwargs):
            if 'pg_dump' in args:
                archives.append(Path(kwargs['stdout'].name))
                self.assertEqual(archives[-1].stat().st_mode & 0o777, 0o600)
            if 'pg_restore' in args:
                self.assertIn('--single-transaction', args)
                self.assertIn('--exit-on-error', args)
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0)
        with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}), \
             patch.object(restore, 'Database', side_effect=[source, target]), \
             patch.object(restore, 'exercise_schema'), \
             patch.object(restore, 'fingerprints', return_value={'tables': '47'}), \
             patch.object(restore.subprocess, 'run', side_effect=command):
            with self.assertRaises(subprocess.CalledProcessError):
                restore.run('kc-tenant-test-db-fixture')
        source.close.assert_called_once()
        target.close.assert_called_once()
        self.assertEqual(len(archives), 1)
        self.assertFalse(archives[0].exists())

    def test_fingerprint_drift_cannot_be_reported_as_a_success(self):
        source, target = self.fixtures()
        before = {'tables': '47', 'rows': 'synthetic-before'}
        after = {'tables': '47', 'rows': 'synthetic-changed'}
        with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}), \
             patch.object(restore, 'Database', side_effect=[source, target]), \
             patch.object(restore, 'exercise_schema'), \
             patch.object(restore, 'fingerprints', side_effect=[before, after]), \
             patch.object(restore, 'check_invariants') as invariants, \
             patch.object(restore.subprocess, 'run'):
            with self.assertRaises(RuntimeError):
                restore.run('kc-tenant-test-db-fixture')
            invariants.assert_not_called()
        source.close.assert_called_once()
        target.close.assert_called_once()

    def test_cleanup_failure_still_attempts_every_owned_database(self):
        source, target = self.fixtures()
        target.close.side_effect = subprocess.CalledProcessError(1, ['dropdb'])
        same = {'tables': '47', 'rows': 'same'}
        with patch.dict(os.environ, {'KC_TENANT_TEST_ACK_ISOLATED': '1'}), \
             patch.object(restore, 'Database', side_effect=[source, target]), \
             patch.object(restore, 'exercise_schema'), \
             patch.object(restore, 'fingerprints', return_value=same), \
             patch.object(restore, 'check_invariants'), \
             patch.object(restore.subprocess, 'run'):
            with self.assertRaisesRegex(RuntimeError, 'cleanup failed'):
                restore.run('kc-tenant-test-db-fixture')
        target.close.assert_called_once()
        source.close.assert_called_once()


if __name__ == '__main__':
    unittest.main()
