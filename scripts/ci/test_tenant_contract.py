"""Negative fixtures prevent the foundation gate from passing on comment matches."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import check_tenant_contract as check

SCHEMA = """
CREATE TABLE IF NOT EXISTS users (
 id UUID PRIMARY KEY, email TEXT, name TEXT,
 platform_role TEXT NOT NULL CHECK(platform_role IN ('root','operator','none')),
 status TEXT, token_version INTEGER, created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ
);
CREATE TABLE IF NOT EXISTS tenants (
 id UUID, owner_user_id UUID NOT NULL, authz_version BIGINT NOT NULL,
 status TEXT CHECK(status IN ('active','inactive'))
);
CREATE TABLE IF NOT EXISTS tenant_memberships (
 tenant_id UUID NOT NULL, user_id UUID NOT NULL,
 tenant_role TEXT CHECK(tenant_role IN ('admin','member')),
 status TEXT CHECK(status IN ('active','suspended','removed')),
 authz_version BIGINT, PRIMARY KEY(tenant_id,user_id)
);
CREATE TABLE IF NOT EXISTS tenant_invitations (
 tenant_id UUID, email TEXT, tenant_role TEXT CHECK(tenant_role IN ('admin','member')),
 token_hash TEXT NOT NULL, status TEXT, expires_at TIMESTAMPTZ, accepted_by UUID
);
CREATE UNIQUE INDEX IF NOT EXISTS invitation_pending
 ON tenant_invitations(tenant_id,lower(email)) WHERE status='pending';
CREATE TABLE IF NOT EXISTS tenant_audit_events (
 scope_type TEXT, tenant_id UUID, actor_user_id UUID, credential_kind TEXT,
 platform_role TEXT, tenant_role TEXT, request_id UUID, action TEXT, result TEXT, metadata JSONB
);
"""
INVENTORY = """table\tclassification
users\tplatform_identity
tenants\ttenant_control
tenant_memberships\ttenant_control
tenant_invitations\ttenant_control
tenant_audit_events\tscoped_audit
"""


class ContractTests(unittest.TestCase):
    def issues(self, schema=SCHEMA, inventory=INVENTORY):
        return check.schema_issues(schema, inventory)[0]

    def test_complete_final_foundation(self):
        self.assertEqual(self.issues(), [])
        self.assertEqual(check.schema_issues(SCHEMA, INVENTORY)[1], 5)

    def test_retired_user_columns_are_rejected(self):
        for field in ('tenant_id UUID', 'role TEXT'):
            self.assertTrue(self.issues(SCHEMA.replace('email TEXT', field + ', email TEXT')))

    def test_domains_cannot_gain_legacy_or_platform_tenant_authority(self):
        self.assertTrue(self.issues(SCHEMA.replace("'root','operator','none'", "'system','admin','user'")))
        self.assertTrue(self.issues(SCHEMA.replace("'admin','member'", "'root','admin','member'")))
        self.assertTrue(self.issues(SCHEMA.replace("'active','suspended','removed'", "'active','suspended','revoked'")))

    def test_required_ownership_and_membership_primary_key(self):
        self.assertTrue(self.issues(SCHEMA.replace('owner_user_id UUID NOT NULL', 'owner_user_id UUID')))
        self.assertTrue(self.issues(SCHEMA.replace('PRIMARY KEY(tenant_id,user_id)', 'UNIQUE(tenant_id,user_id)')))
        self.assertTrue(self.issues(SCHEMA.replace('token_version INTEGER,', '')))

    def test_invitation_index_must_be_unique_and_pending_and_tokens_hashed(self):
        self.assertTrue(self.issues(SCHEMA.replace('CREATE UNIQUE INDEX', 'CREATE INDEX')))
        self.assertTrue(self.issues(SCHEMA.replace("WHERE status='pending'", "WHERE status='accepted'")))
        self.assertTrue(self.issues(SCHEMA.replace("WHERE status='pending'", "WHERE status='pending' AND FALSE")))
        self.assertTrue(self.issues(SCHEMA.replace('token_hash TEXT NOT NULL', 'token_hash TEXT')))
        for field in ('token TEXT', 'plaintext_token TEXT', 'platform_role TEXT'):
            self.assertTrue(self.issues(SCHEMA.replace('token_hash TEXT NOT NULL', 'token_hash TEXT NOT NULL, ' + field)))

    def test_every_table_has_exactly_one_declared_classification(self):
        self.assertTrue(self.issues(inventory=INVENTORY + 'users\tplatform_identity\n'))
        self.assertTrue(self.issues(inventory=INVENTORY.replace('users\tplatform_identity\n', '')))
        self.assertTrue(self.issues(inventory=INVENTORY + 'phantom\ttenant_resource\n'))
        self.assertTrue(self.issues(inventory=INVENTORY.replace('platform_identity', 'unknown')))
        self.assertTrue(self.issues(inventory='wrong\theader\n'))
        self.assertTrue(self.issues(SCHEMA + 'CREATE TABLE IF NOT EXISTS extra (id UUID);'))

    def test_sql_comments_function_bodies_and_quoted_values_do_not_add_tables(self):
        noise = """
-- CREATE TABLE IF NOT EXISTS comment_only (role TEXT);
/* nested /* CREATE TABLE x (id UUID); */ comment */
DO $body$ BEGIN RAISE NOTICE 'CREATE TABLE missing (id UUID)'; END $body$;
SELECT 'CREATE TABLE fake (id UUID);';
"""
        self.assertEqual(self.issues(SCHEMA+noise), [])
        quoted = SCHEMA.replace('IF NOT EXISTS users', 'IF NOT EXISTS public."users"')
        self.assertEqual(self.issues(quoted), [])
        with self.assertRaises(ValueError):
            self.issues(SCHEMA.replace('IF NOT EXISTS users', 'users'))
        with self.assertRaises(ValueError):
            self.issues(SCHEMA + '/* truncated')
        with self.assertRaises(ValueError):
            self.issues(SCHEMA + 'CREATE TABLE IF NOT EXISTS')

    def test_unsupported_table_modifiers_and_quoted_case_cannot_hide_inventory_drift(self):
        for modifier in ('UNLOGGED', 'TEMP', 'TEMPORARY'):
            with self.assertRaises(ValueError):
                self.issues(SCHEMA + f'CREATE {modifier} TABLE IF NOT EXISTS hidden(id UUID);')
        self.assertTrue(self.issues(SCHEMA.replace('IF NOT EXISTS users', 'IF NOT EXISTS "Users"')))

    def test_actual_retired_symbols_are_rejected_across_whitespace_comments(self):
        for source in ('Permission::SystemAdmin', 'UserRole :: Admin', 'UserRole/*nested /*x*/ x*/::System',
                       'AuthContext::is_admin()', 'AuthExtractor :: is_admin ()', 'struct AssignableUserRole;',
                       'enum UserRole {Admin}', 'fn is_admin(&self) -> bool {}', 'auth.is_admin()'):
            self.assertTrue(check.rust_issues(source), source)

    def test_documentation_raw_literals_characters_and_lifetimes_are_not_authority(self):
        source = '''
// Permission::SystemAdmin
/* UserRole::Admin /* AuthContext::is_admin */ */
fn sample<'a>(text: &'a str) { let x = "UserRole::System";
let raw = r###"AuthExtractor::is_admin()"###;
let quote = '\\''; let lifetime: &'a str = text; }
'''
        self.assertEqual(check.rust_issues(source), [])
        self.assertEqual(check.rust_issues('m.insert("users.role", "Update the user\'s display");'), [])
        with self.assertRaises(ValueError):
            check.rust_issues('let x = r#"unterminated;')

    def test_retired_users_fields_in_sql_not_translation_keys(self):
        self.assertTrue(check.rust_issues('let query = "SELECT users.role FROM users";'))
        self.assertTrue(check.rust_issues('let query = r#"SELECT users . tenant_id FROM users"#;'))
        self.assertEqual(check.rust_issues('let query = "SELECT \'users.role\' FROM users";'), [])
        self.assertEqual(check.rust_issues('let key = "users.role_root";'), [])

    def test_escaped_and_quoted_sql_identifiers_cannot_hide_retired_columns(self):
        self.assertTrue(check.rust_issues(r'let q = "SELECT \"users\".\"role\" FROM users";'))
        self.assertTrue(check.rust_issues(r'let q = "SELECT \u{75}sers.\x72ole FROM users";'))
        self.assertEqual(check.rust_issues("let q = r#\"SELECT 'users.role' FROM users\"#;"), [])

    def test_workflow_runs_the_guard_and_triggers_for_schema_inventory_and_exclusions(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root/'.github/workflows/keycompute.yml').read_text()
        for path in ('crates/keycompute-db/migrations/**', 'docs/tenant-schema-inventory.tsv', '.gitignore'):
            self.assertEqual(workflow.count('      - "' + path + '"'), 2)
        self.assertIn('run: python3 scripts/ci/check_tenant_contract.py', workflow)
        self.assertIn("python3 -m unittest discover -s scripts/ci -p 'test_*.py'", workflow)

    def test_repository_rejects_extra_migrations_and_foreign_workspace_members(self):
        with tempfile.TemporaryDirectory(prefix='kc-contract-fixture-') as temp:
            root = Path(temp)
            (root/'crates/keycompute-db/migrations').mkdir(parents=True)
            (root/'docs').mkdir()
            (root/'crates/keycompute-db/migrations/001_init.sql').write_text(SCHEMA)
            (root/'docs/tenant-schema-inventory.tsv').write_text(INVENTORY)
            (root/'Cargo.toml').write_text('[workspace]\nmembers = ["crates/example"]\n')
            (root/'.gitignore').write_text('new/\n')
            with patch.object(check.subprocess, 'check_output', return_value=b''):
                self.assertTrue(check.check_repository(root)['passed'])
                (root/'Cargo.toml').write_text('[workspace]\nmembers = []\nexclude = ["new"]\n')
                self.assertTrue(check.check_repository(root)['passed'])
                extra = root/'crates/keycompute-db/migrations/002_compat.sql'
                extra.write_text('-- should never be loaded')
                self.assertFalse(check.check_repository(root)['passed'])
                extra.unlink()
                (root/'Cargo.toml').write_text('[workspace]\nmembers = ["new"]\n')
                self.assertFalse(check.check_repository(root)['passed'])
                (root/'Cargo.toml').write_text('[workspace]\nmembers = []\n')
            with patch.object(check.subprocess, 'check_output', return_value=b'new/go.mod\0'):
                self.assertFalse(check.check_repository(root)['passed'])
            (root/'.gitignore').write_text('')
            with patch.object(check.subprocess, 'check_output', return_value=b''):
                self.assertFalse(check.check_repository(root)['passed'])


if __name__ == '__main__':
    unittest.main()
