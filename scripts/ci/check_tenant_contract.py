#!/usr/bin/env python3
"""Read-only foundation guard, not certification of all handler/DAO authorization.

Check the single greenfield schema, declared table inventory, retired Rust
symbols and the separately deployed Go identity boundary. No DB, secret or
network access is performed. Findings fail closed; results never print SQL values.
"""
from __future__ import annotations

import argparse
import csv
from dataclasses import dataclass
import io
import json
from pathlib import Path
import re
import subprocess
import sys


@dataclass(frozen=True)
class Token:
    kind: str
    value: str


DOLLAR = re.compile(r'\$(?:[A-Za-z_][A-Za-z_0-9]*)?\$')
RAW = re.compile(r'(?:b|c)?r(#{0,255})"')
CHAR = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\])'")
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*|[0-9]+|::|[^\s]')


def rust_literal(value: str) -> str:
    result: list[str] = []
    i = 0
    simple = {'n': '\n', 'r': '\r', 't': '\t', '0': '\0', '\\': '\\', '"': '"', "'": "'"}
    while i < len(value):
        if value[i] != '\\':
            result.append(value[i])
            i += 1
            continue
        i += 1
        if i >= len(value):
            raise ValueError('incomplete Rust escape')
        escaped = value[i]
        if escaped in simple:
            result.append(simple[escaped])
            i += 1
        elif escaped == 'x' and i + 2 < len(value):
            result.append(chr(int(value[i+1:i+3], 16)))
            i += 3
        elif value.startswith('u{', i):
            end = value.find('}', i+2)
            if end < 0:
                raise ValueError('invalid Rust Unicode escape')
            result.append(chr(int(value[i+2:end].replace('_', ''), 16)))
            i = end + 1
        elif escaped.isspace():
            while i < len(value) and value[i].isspace():
                i += 1
        else:
            raise ValueError('unsupported Rust escape')
    return ''.join(result)


def tokenize(text: str, language: str) -> list[Token]:
    """Small lexical scanner: comments/quoted bodies are never executable tokens."""
    result: list[Token] = []
    i = 0
    while i < len(text):
        if text[i].isspace():
            i += 1
        elif text.startswith('--' if language == 'sql' else '//', i):
            end = text.find('\n', i)
            i = len(text) if end < 0 else end + 1
        elif text.startswith('/*', i):
            depth, i = 1, i + 2
            while depth and i < len(text):
                if text.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif text.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise ValueError('unterminated block comment')
        elif language == 'sql' and (match := DOLLAR.match(text, i)):
            tag = match.group()
            end = text.find(tag, i + len(tag))
            if end < 0:
                raise ValueError('unterminated dollar-quoted SQL body')
            result.append(Token('literal', text[i + len(tag):end]))
            i = end + len(tag)
        elif language == 'rust' and (match := RAW.match(text, i)):
            closing = '"' + match.group(1)
            end = text.find(closing, i + len(match.group()))
            if end < 0:
                raise ValueError('unterminated raw Rust string')
            result.append(Token('literal', text[i + len(match.group()):end]))
            i = end + len(closing)
        elif text[i] == '"' or (language == 'sql' and text[i] == "'"):
            quote, start = text[i], i + 1
            i += 1
            while i < len(text):
                if language == 'rust' and text[i] == '\\':
                    i += 2
                elif text[i] == quote:
                    if language == 'sql' and text[i:i+2] == quote * 2:
                        i += 2
                    else:
                        break
                else:
                    i += 1
            if i >= len(text):
                raise ValueError('unterminated quoted value')
            value = text[start:i]
            if language == 'rust':
                value = rust_literal(value)
            kind = 'identifier' if language == 'sql' and quote == '"' else 'literal'
            result.append(Token(kind, value))
            i += 1
        elif language == 'rust' and (match := CHAR.match(text, i)):
            i += len(match.group())
        elif match := WORD.match(text, i):
            value = match.group()
            result.append(Token('identifier', value.lower() if language == 'sql' else value))
            i += len(value)
        else:
            raise ValueError('unrecognized source character')
    return result


def words(tokens: list[Token]) -> list[str]:
    return [t.value for t in tokens]


def contains(values: list[str], pattern: list[str]) -> bool:
    return any(values[i:i+len(pattern)] == pattern for i in range(len(values)-len(pattern)+1))


def table_definitions(tokens: list[Token]) -> dict[str, list[Token]]:
    tables: dict[str, list[Token]] = {}
    i = 0
    while i < len(tokens):
        if words(tokens[i:i+3]) in (['create', 'unlogged', 'table'], ['create', 'temp', 'table'], ['create', 'temporary', 'table']):
            raise ValueError('unsupported table modifier in final greenfield schema')
        if tokens[i].kind != 'identifier' or words(tokens[i:i+2]) != ['create', 'table']:
            i += 1
            continue
        if words(tokens[i+2:i+5]) != ['if', 'not', 'exists']:
            raise ValueError('CREATE TABLE must be replay safe')
        name_index = i + 5
        if words(tokens[name_index:name_index+2]) == ['public', '.']:
            name_index += 2
        if name_index + 1 >= len(tokens):
            raise ValueError('incomplete table declaration')
        name = tokens[name_index].value
        if name in tables or tokens[name_index+1].value != '(':
            raise ValueError('duplicate or unsupported table definition')
        start, j, depth = name_index + 2, name_index + 2, 1
        while j < len(tokens) and depth:
            if tokens[j].kind == 'identifier':
                depth += (tokens[j].value == '(') - (tokens[j].value == ')')
            j += 1
        if depth:
            raise ValueError('unterminated table definition')
        tables[name] = tokens[start:j-1]
        i = j
    return tables


def columns(tokens: list[Token]) -> dict[str, list[str]]:
    entries, current, depth = [], [], 0
    for token in tokens:
        if token.kind == 'identifier' and token.value == ',' and depth == 0:
            entries.append(current)
            current = []
            continue
        current.append(token)
        if token.kind == 'identifier':
            depth += (token.value == '(') - (token.value == ')')
    entries.append(current)
    constraints = {'constraint', 'primary', 'foreign', 'unique', 'check', 'exclude'}
    return {entry[0].value: words(entry[1:]) for entry in entries if entry and entry[0].value not in constraints}


def domain(tokens: list[Token], field: str) -> set[str]:
    for i in range(len(tokens)-3):
        if words(tokens[i:i+3]) == [field, 'in', '('] and all(t.kind == 'identifier' for t in tokens[i:i+3]):
            result: set[str] = set()
            for item in tokens[i+3:]:
                if item.kind == 'identifier' and item.value == ')':
                    return result
                if item.kind == 'literal':
                    result.add(item.value)
    return set()


def schema_issues(schema: str, inventory: str) -> tuple[list[str], int]:
    tokens = tokenize(schema, 'sql')
    tables = table_definitions(tokens)
    failures: list[str] = []
    required = {
        'users': {'id', 'email', 'name', 'platform_role', 'status', 'token_version', 'created_at', 'updated_at'},
        'tenants': {'id', 'owner_user_id', 'authz_version', 'status'},
        'tenant_memberships': {'tenant_id', 'user_id', 'tenant_role', 'status', 'authz_version'},
        'tenant_invitations': {'tenant_id', 'email', 'tenant_role', 'token_hash', 'status', 'expires_at', 'accepted_by'},
        'tenant_audit_events': {'scope_type', 'tenant_id', 'actor_user_id', 'credential_kind', 'platform_role', 'tenant_role', 'request_id', 'action', 'result', 'metadata'},
    }
    for table, fields in required.items():
        present = columns(tables.get(table, []))
        for missing in sorted(fields - present.keys()):
            failures.append(f'{table}: missing required column {missing}')
    for forbidden in {'tenant_id', 'role'} & columns(tables.get('users', [])).keys():
        failures.append(f'users: retired ownership column {forbidden}')
    for table, field, allowed in [
        ('users', 'platform_role', {'root', 'operator', 'none'}),
        ('tenant_memberships', 'tenant_role', {'admin', 'member'}),
        ('tenant_memberships', 'status', {'active', 'suspended', 'removed'}),
        ('tenant_invitations', 'tenant_role', {'admin', 'member'}),
        ('tenants', 'status', {'active', 'inactive'}),
    ]:
        if domain(tables.get(table, []), field) != allowed:
            failures.append(f'{table}: {field} domain differs from the final contract')
    for table, field in [('tenants', 'owner_user_id'), ('tenants', 'authz_version'),
                         ('tenant_memberships', 'tenant_id'), ('tenant_memberships', 'user_id'),
                         ('tenant_invitations', 'token_hash')]:
        if not contains(columns(tables.get(table, [])).get(field, []), ['not', 'null']):
            failures.append(f'{table}.{field}: NOT NULL required')
    if not contains(words(tables.get('tenant_memberships', [])), ['primary', 'key', '(', 'tenant_id', ',', 'user_id', ')']):
        failures.append('tenant_memberships: composite primary key required')
    pending_index = ['on', 'tenant_invitations', '(', 'tenant_id', ',', 'lower', '(', 'email', ')', ')', 'where', 'status', '=', 'pending']
    unique_pending = False
    for i in range(len(tokens)-3):
        if words(tokens[i:i+3]) == ['create', 'unique', 'index']:
            end = next((j for j in range(i, len(tokens)) if tokens[j].value == ';' and tokens[j].kind == 'identifier'), len(tokens))
            unique_pending |= words(tokens[i:end])[-len(pending_index):] == pending_index
    if not unique_pending:
        failures.append('tenant_invitations: unique pending-email index contract missing')
    forbidden = {'token', 'plain_token', 'plaintext_token', 'platform_role'}
    if forbidden & columns(tables.get('tenant_invitations', [])).keys():
        failures.append('tenant_invitations: raw token or platform authority column is forbidden')
    reader = csv.DictReader(io.StringIO(inventory), delimiter='\t')
    if reader.fieldnames != ['table', 'classification']:
        return failures + ['schema inventory: invalid header'], len(tables)
    classifications = {'platform_identity', 'tenant_control', 'scoped_audit', 'user_tenant_resource',
                       'tenant_resource', 'parent_scoped_child', 'explicit_platform_or_tenant', 'platform_control'}
    entries: dict[str, str] = {}
    for row in reader:
        name, kind = row.get('table'), row.get('classification')
        if not name or name in entries or kind not in classifications or None in row:
            failures.append('schema inventory: duplicate, malformed or unclassified row')
        if name:
            entries[name] = kind or ''
    failures.extend(f'schema inventory: missing {name}' for name in sorted(tables.keys()-entries.keys()))
    failures.extend(f'schema inventory: stale {name}' for name in sorted(entries.keys()-tables.keys()))
    return failures, len(tables)


RETIRED = [
    ['Permission', '::', 'SystemAdmin'], ['UserRole', '::', 'System'], ['UserRole', '::', 'Admin'],
    ['AuthExtractor', '::', 'is_admin'], ['AuthContext', '::', 'is_admin'], ['AssignableUserRole'],
    ['enum', 'UserRole'], ['fn', 'is_admin'], ['.', 'is_admin', '('],
]


def rust_issues(source: str) -> list[str]:
    tokens = tokenize(source, 'rust')
    executable = [token.value if token.kind == 'identifier' else '<literal>' for token in tokens]
    issues = ['retired authorization symbol: ' + ''.join(pattern) for pattern in RETIRED if contains(executable, pattern)]
    for token in tokens:
        if token.kind == 'literal' and re.search(r'^\s*(?:SELECT|UPDATE|INSERT|DELETE|WITH)\b', token.value, re.I) and re.search(r'\busers\b', token.value, re.I) and re.search(r'\b(?:tenant_id|role)\b', token.value, re.I):
            # This deliberately checks the explicit users table spelling only;
            # alias/object authorization requires actual DAO/HTTP security tests.
            sql = tokenize(token.value, 'sql')
            code = [t.value if t.kind == 'identifier' else '<literal>' for t in sql]
            if any(contains(code, ['users', '.', field]) for field in ('tenant_id', 'role')):
                issues.append('retired users ownership reference in SQL literal')
    return sorted(set(issues))


def check_repository(root: Path) -> dict:
    files = subprocess.check_output(['git', '-C', str(root), 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], timeout=20).decode().split('\0')
    files = [name for name in files if name]
    failures, count = schema_issues(
        (root/'crates/keycompute-db/migrations/001_init.sql').read_text(),
        (root/'docs/tenant-schema-inventory.tsv').read_text(),
    )
    migrations = {p.name for p in (root/'crates/keycompute-db/migrations').glob('*.sql')}
    if migrations != {'001_init.sql'}:
        failures.append('only the complete greenfield 001_init.sql may exist')
    rust_files = [name for name in files if name.endswith('.rs') and name.startswith(('crates/', 'packages/'))]
    for name in rust_files:
        try:
            if (root/name).is_symlink():
                failures.append(f'{name}: source symlink is unsupported by this guard')
                continue
            failures.extend(f'{name}: {issue}' for issue in rust_issues((root/name).read_text()))
        except ValueError:
            failures.append(f'{name}: source scanner could not establish a safe parse')
    if any(name.startswith('new/') for name in files):
        failures.append('independent new/ service is unexpectedly tracked in the Rust repository')
    if 'new/' not in (root/'.gitignore').read_text().splitlines():
        failures.append('independent new/ checkout must remain excluded')
    cargo = (root/'Cargo.toml').read_text()
    workspace = re.search(r'(?ms)^\[workspace\]\s*$(.*?)(?=^\[|\Z)', cargo)
    members = re.search(r'(?ms)^members\s*=\s*\[(.*?)\]', workspace.group(1)) if workspace else None
    if members and re.search(r'"(?:\./)?new(?:/[^"\n]*)?"', members.group(1)):
        failures.append('independent Go service must not be a Cargo workspace member')
    return {'passed': not failures, 'schema_tables': count, 'rust_files_scanned': len(rust_files),
            'findings': failures, 'scope': 'foundation/schema-inventory/retired-symbols/source-boundary',
            'not_covered': ['arbitrary SQL aliases or dynamically generated SQL', 'all object-level DAO predicates', 'live Go deployments',
                            'frontend/browser acceptance', 'full release and snapshot restore']}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        result = check_repository(args.root.resolve())
    except (OSError, ValueError, subprocess.SubprocessError):
        print(json.dumps({'passed': False, 'findings': ['contract sources could not be validated']}))
        return 2
    print(json.dumps(result, indent=2))
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
