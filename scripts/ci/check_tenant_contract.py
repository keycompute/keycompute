#!/usr/bin/env python3
"""Read-only foundation guard, not certification of all handler/DAO authorization.

Check the single greenfield schema, declared table inventory and retired Rust
symbols. No DB, secret or network access is performed. Findings fail closed;
results never print SQL values.
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

ROUTE_INVENTORY_FIELDS = [
    'existing_path',
    'source_file',
    'line_at_audit',
    'resource_category',
    'authority_contract',
    'canonical_target',
]
ROUTE_CATEGORIES = {
    'global_shared_resource',
    'platform_resource',
    'platform_resource_or_explicit_tenant_target',
    'tenant_resource',
    'user_owned_resource',
}
ROUTE_AUTHORITY_HINTS = {
    'global_shared_resource': ('tenant', 'owner', 'shared', 'grant', 'credential'),
    'platform_resource': ('platform', 'root', 'operator', 'identity', 'public'),
    'platform_resource_or_explicit_tenant_target': ('platform', 'root', 'tenant', 'target'),
    'tenant_resource': ('tenant', 'membership', 'admin', 'selected'),
    'user_owned_resource': ('owner', 'self', 'tenant', 'credential', 'node'),
}
ROUTE_CANONICAL_PREFIXES = {
    'global_shared_resource': ('/api/v1/platform/', '/api/v1/tenants/', '/api/v1/me/'),
    'platform_resource': ('/api/v1/platform/',),
    'platform_resource_or_explicit_tenant_target': ('/api/v1/platform/', '/api/v1/tenants/', '/api/v1/me/'),
    'tenant_resource': ('/api/v1/tenants/', '/api/v1/platform/'),
    'user_owned_resource': ('/api/v1/me/', '/api/v1/tenants/', '/api/v1/platform/'),
}
IGNORED_ROUTE_FILES = {'console_tests.rs', 'drain_tests.rs', 'tests.rs'}
# These literals belong only to inline unit-test routers, not production endpoints.
IGNORED_ROUTE_LITERALS = {
    '/',
    '/other',
    '/rate-limited',
    '/standalone-self-service',
    '/work',
    '/api/v1/admin/users',
}
ROUTE_LITERAL = re.compile(r'\.(?:route|route_service)\(\s*"([^"\\]+)"')

CACHE_JOB_INVENTORY_FIELDS = [
    'path',
    'line_at_audit',
    'symbol',
    'kind',
    'classification',
    'required_scope',
    'acceptance',
]
CACHE_JOB_KINDS = {
    'cache-boundary',
    'cache-key',
    'cache-operation',
    'command',
    'configuration-command',
    'control-plane',
    'deferred-intent',
    'idempotent-command',
    'job',
    'live-stream-authority',
    'query',
    'read-model',
    'request-identity',
}
CACHE_JOB_CLASSIFICATIONS = {
    'platform configuration',
    'platform identities and tenant lifecycle',
    'platform identity or tenant credential resource',
    'platform infrastructure / caller-scoped primitive',
    'platform infrastructure / request-bound transport',
    'platform infrastructure with scoped work items',
    'platform operational metadata',
    'platform protected configuration',
    'platform resource',
    'platform resource or explicitly scoped tenant administration',
    'platform tenant lifecycle',
    'tenant resource',
    'tenant resource / platform shared price',
    'tenant resource or explicit global shared resource',
    'tenant/user resource',
    'tenant/user resource or explicit platform view',
    'user-owned financial resource',
    'user-owned payment resource',
    'user-owned resource',
    'user-owned tenant resource',
}
CACHE_JOB_SCOPE_HINTS = {
    'platform configuration': ('platform', 'root', 'global', 'configuration', 'secret', 'scope'),
    'platform identities and tenant lifecycle': ('platform', 'root', 'tenant', 'user', 'owner', 'scope', 'snapshot'),
    'platform identity or tenant credential resource': ('identity', 'credential', 'tenant', 'actor', 'owner', 'authorization', 'platform', 'scope'),
    'platform infrastructure / caller-scoped primitive': ('caller', 'immutable', 'work', 'scope', 'not authorization'),
    'platform infrastructure / request-bound transport': ('request', 'caller', 'scope', 'stream'),
    'platform infrastructure with scoped work items': ('tenant', 'owner', 'work', 'scope', 'scheduler', 'item'),
    'platform operational metadata': ('platform', 'root', 'operator', 'tenant', 'target', 'canonical', 'fresh', 'read-only'),
    'platform protected configuration': ('platform', 'root', 'global', 'configuration', 'secret', 'scope'),
    'platform resource': ('platform', 'root', 'operator', 'owner', 'scope', 'host', 'resource', 'configuration'),
    'platform resource or explicitly scoped tenant administration': ('platform', 'tenant', 'operator', 'owner', 'scope', 'resource'),
    'platform tenant lifecycle': ('platform', 'tenant', 'user', 'owner', 'scope', 'snapshot'),
    'tenant resource': ('tenant', 'owner', 'consumer', 'platform', 'shared', 'price', 'scope', 'account', 'node'),
    'tenant resource / platform shared price': ('tenant', 'owner', 'consumer', 'platform', 'shared', 'price', 'scope', 'account', 'node'),
    'tenant resource or explicit global shared resource': ('tenant', 'owner', 'consumer', 'platform', 'shared', 'scope', 'account', 'node'),
    'tenant/user resource': ('tenant', 'user', 'actor', 'owner', 'grant', 'credential', 'authorization', 'platform', 'scope'),
    'tenant/user resource or explicit platform view': ('tenant', 'user', 'actor', 'owner', 'grant', 'credential', 'authorization', 'platform', 'scope'),
    'user-owned financial resource': ('tenant', 'user', 'owner', 'usage', 'billing', 'currency', 'request', 'scope', 'snapshot'),
    'user-owned payment resource': ('tenant', 'user', 'owner', 'credential', 'order', 'billing', 'request', 'scope'),
    'user-owned resource': ('tenant', 'user', 'owner', 'credential', 'request', 'scope', 'task', 'node'),
    'user-owned tenant resource': ('tenant', 'user', 'owner', 'scope', 'snapshot', 'authority'),
}
OBJECT_COVERAGE_INVENTORY_FIELDS = [
    'route',
    'resource_category',
    'handler_source',
    'handler_symbol',
    'dao_source',
    'dao_symbol',
    'test_file',
    'test_symbol',
]
CACHE_JOB_COVERAGE_FIELDS = ['path', 'symbol', 'test_file', 'test_symbol']
DYNAMIC_SQL_REVIEW_FIELDS = ['path', 'symbol', 'required_tokens', 'test_file', 'test_symbol']
OBJECT_COVERAGE_FAMILIES = {
    'tenant-control': re.compile(r'^/api/v1/tenants/\{tenant_id\}/(?:members|invitations|audit-events)'),
    'providers': re.compile(r'^/api/v1/tenants/\{tenant_id\}/accounts'),
    'keys': re.compile(r'^/api/v1/tenants/\{tenant_id\}/keys'),
    'pricing': re.compile(r'^/api/v1/tenants/\{tenant_id\}/pricing'),
    'payments': re.compile(r'^/api/v1/tenants/\{tenant_id\}/payments/'),
    'nodes': re.compile(r'^/api/v1/tenants/\{tenant_id\}/nodes'),
    'tasks': re.compile(r'^/api/v1/tenants/\{tenant_id\}/tasks'),
    'usage': re.compile(r'^/api/v1/tenants/\{tenant_id\}/usage'),
    'distribution': re.compile(r'^/api/v1/tenants/\{tenant_id\}/distribution'),
    'responses': re.compile(r'^/api/v1/tenants/\{tenant_id\}/responses'),
    'tips': re.compile(r'^/api/v1/tenants/\{tenant_id\}/tips'),
    'wallets': re.compile(r'^/api/v1/tenants/\{tenant_id\}/balances/'),
    'platform-identity': re.compile(r'^/api/v1/platform/(?:users|tenants)'),
}


def release_gate_issues(root: Path) -> list[str]:
    """Require an explicit, non-destructive release/restore readiness record."""
    failures: list[str] = []
    rehearsal = root / 'scripts/tests/tenant_restore_rehearsal.py'
    browser_smoke = root / 'scripts/tests/tenant_backend_browser.mjs'
    gate_doc = root / 'docs/tenant-release-gate.md'
    if not rehearsal.is_file():
        failures.append('release gate: tenant restore rehearsal script is missing')
    if not browser_smoke.is_file():
        failures.append('release gate: real backend browser smoke script is missing')
    if not gate_doc.is_file():
        failures.append('release gate: tenant release gate document is missing')
        return failures
    text = gate_doc.read_text()
    normalized = re.sub(r'\s+', ' ', text).lower()
    required_phrases = {
        'kc_tenant_test_ack_isolated=1': 'isolated restore acknowledgement is missing',
        'not a production backup': 'production backup disclaimer is missing',
        'real backend browser': 'real backend browser gate is missing',
        'rollback': 'rollback procedure is missing',
    }
    for phrase, message in required_phrases.items():
        if phrase not in normalized:
            failures.append(f'release gate: {message}')
    return failures


def production_route_sources(root: Path) -> list[Path]:
    source_root = root / 'crates/keycompute-server/src'
    if not source_root.is_dir():
        return []
    return [
        source
        for source in sorted(source_root.rglob('*.rs'))
        if not source.name.endswith('_tests.rs') and source.name not in IGNORED_ROUTE_FILES
    ]


def route_builder_issues(root: Path) -> list[str]:
    """Reject route composition that the literal inventory cannot prove."""
    failures: list[str] = []
    for source in production_route_sources(root):
        text = source.read_text()
        tokens = tokenize(text, 'rust')
        # A macro can emit routes without leaving a literal declaration at the
        # call site. Reject route-building macros until they are represented by
        # an explicit generated-route inventory rather than silently treating
        # their template as a production endpoint.
        macro_route = False
        for start in range(len(tokens) - 3):
            if tokens[start].value != 'macro_rules' or tokens[start + 1].value != '!':
                continue
            if tokens[start + 2].kind != 'identifier' or tokens[start + 3].value != '{':
                continue
            depth = 1
            cursor = start + 4
            while cursor < len(tokens) and depth:
                value = tokens[cursor].value
                if value == '{':
                    depth += 1
                elif value == '}':
                    depth -= 1
                elif value == '.' and cursor + 2 < len(tokens) and tokens[cursor + 1].value in {'route', 'route_service'} and tokens[cursor + 2].value == '(':
                    macro_route = True
                cursor += 1
            if macro_route:
                break
        if macro_route:
            relative = source.relative_to(root).as_posix()
            failures.append(f'route inventory: macro-generated route builder is unsupported in {relative}')
        values = words(tokens)
        relative = source.relative_to(root).as_posix()
        for index in range(len(tokens) - 2):
            if values[index] != '.' or values[index + 2] != '(':
                continue
            method = values[index + 1]
            if method in {'nest', 'nest_service'}:
                failures.append(f'route inventory: unsupported nested route builder in {relative}')
                continue
            if method not in {'route', 'route_service'}:
                continue
            depth = 1
            comma = None
            cursor = index + 3
            while cursor < len(tokens) and depth:
                value = values[cursor]
                if value == '(':
                    depth += 1
                elif value == ')':
                    depth -= 1
                elif value == ',' and depth == 1:
                    comma = cursor
                    break
                cursor += 1
            # One-argument methods such as the runtime provider router are not
            # Axum route declarations.
            if comma is None:
                continue
            first_argument = tokens[index + 3:comma]
            if not (
                len(first_argument) == 1
                and first_argument[0].kind == 'literal'
                and first_argument[0].value.startswith('/')
            ):
                failures.append(f'route inventory: dynamic route path is unsupported in {relative}')
    return sorted(set(failures))


def production_routes(root: Path) -> dict[str, tuple[str, int]]:
    routes: dict[str, tuple[str, int]] = {}
    for source in production_route_sources(root):
        text = source.read_text()
        relative = source.relative_to(root).as_posix()
        for match in ROUTE_LITERAL.finditer(text):
            route = match.group(1)
            if route in IGNORED_ROUTE_LITERALS or route.startswith('/api/v1/test/'):
                continue
            if not (
                route.startswith(('/api/v1/', '/v1/', '/pt/', '/nt/', '/node/'))
                or route in {'/health', '/ready'}
            ):
                continue
            routes.setdefault(route, (relative, text[:match.start()].count('\n') + 1))
    return routes


def route_inventory_issues(root: Path) -> tuple[list[str], int]:
    inventory_path = root / 'docs/tenant-route-inventory.tsv'
    if not inventory_path.is_file():
        return ['route inventory: missing docs/tenant-route-inventory.tsv'], 0
    with inventory_path.open(newline='') as handle:
        rows = list(csv.DictReader(handle, delimiter='\t'))
    failures: list[str] = route_builder_issues(root)
    if not rows:
        header = inventory_path.read_text().splitlines()[0].split('\t') if inventory_path.read_text().splitlines() else []
        if header != ROUTE_INVENTORY_FIELDS:
            failures.append('route inventory: invalid header')
    elif list(rows[0].keys()) != ROUTE_INVENTORY_FIELDS:
        failures.append('route inventory: invalid header')
    seen: set[str] = set()
    for row in rows:
        path = row.get('existing_path', '')
        if not path or path in seen:
            failures.append(f'route inventory: duplicate or empty path {path!r}')
            continue
        seen.add(path)
        if any(not row.get(field) for field in ROUTE_INVENTORY_FIELDS):
            failures.append(f'route inventory: incomplete row {path!r}')
        if row.get('resource_category') not in ROUTE_CATEGORIES:
            failures.append(f'route inventory: invalid resource category for {path!r}')
        else:
            contract = row.get('authority_contract', '').lower()
            if not any(hint in contract for hint in ROUTE_AUTHORITY_HINTS[row['resource_category']]):
                failures.append(
                    f'route inventory: authority contract does not describe '
                    f"{row['resource_category']} scope for {path!r}"
                )
            target = row.get('canonical_target', '')
            if target != 'unchanged' and not target.startswith(ROUTE_CANONICAL_PREFIXES[row['resource_category']]):
                failures.append(
                    f'route inventory: canonical target namespace does not match '
                    f"{row['resource_category']} for {path!r}"
                )
        try:
            if int(row.get('line_at_audit', '')) < 0:
                raise ValueError
        except ValueError:
            failures.append(f'route inventory: invalid line for {path!r}')
        source_file = row.get('source_file', '')
        source = root / source_file
        if not source_file.startswith('crates/keycompute-server/src/'):
            failures.append(f'route inventory: source outside server tree for {path!r}: {source_file}')
        elif not source.is_file():
            failures.append(f'route inventory: missing source file for {path!r}: {source_file}')
        elif path not in source.read_text():
            failures.append(f'route inventory: route literal not found for {path!r}: {source_file}')
    actual = production_routes(root)
    for path in sorted(actual.keys() - seen):
        source, line = actual[path]
        failures.append(f'route inventory: unclassified route {path!r} at {source}:{line}')
    for path in sorted(seen - actual.keys()):
        failures.append(f'route inventory: stale route {path!r}')
    return failures, len(actual)


def cache_job_inventory_issues(root: Path, tracked_files: set[str] | None = None) -> tuple[list[str], int]:
    """Validate cache/job inventory shape and its declared scope semantics."""
    inventory_path = root / 'docs/tenant-cache-job-inventory.tsv'
    if not inventory_path.is_file():
        return ['cache/job inventory: missing docs/tenant-cache-job-inventory.tsv'], 0
    with inventory_path.open(newline='') as handle:
        reader = csv.DictReader(handle, delimiter='\t')
        failures: list[str] = []
        if reader.fieldnames != CACHE_JOB_INVENTORY_FIELDS:
            return ['cache/job inventory: invalid header'], 0
        rows = list(reader)

    seen: set[tuple[str, str, str, str]] = set()
    for row_number, row in enumerate(rows, start=2):
        if None in row:
            failures.append(f'cache/job inventory: malformed row {row_number}')
            continue
        if any(not row.get(field, '').strip() for field in CACHE_JOB_INVENTORY_FIELDS):
            failures.append(f'cache/job inventory: incomplete row {row_number}')
            continue

        path = row['path'].strip()
        source = root / path
        path_obj = Path(path)
        if path_obj.is_absolute() or '..' in path_obj.parts:
            failures.append(f'cache/job inventory: path escapes repository at row {row_number}: {path!r}')
            continue
        if not source.is_file():
            failures.append(f'cache/job inventory: missing source file at row {row_number}: {path!r}')
        elif tracked_files is not None and path not in tracked_files:
            failures.append(f'cache/job inventory: source is untracked or ignored at row {row_number}: {path!r}')

        try:
            if int(row['line_at_audit']) < 0:
                raise ValueError
        except ValueError:
            failures.append(f'cache/job inventory: invalid line at row {row_number}')
        if row['kind'] not in CACHE_JOB_KINDS:
            failures.append(f'cache/job inventory: invalid kind at row {row_number}: {row["kind"]!r}')
        if row['classification'] not in CACHE_JOB_CLASSIFICATIONS:
            failures.append(f'cache/job inventory: invalid classification at row {row_number}: {row["classification"]!r}')
        else:
            required_scope = row['required_scope'].lower()
            if not any(hint in required_scope for hint in CACHE_JOB_SCOPE_HINTS[row['classification']]):
                failures.append(
                    f'cache/job inventory: required scope does not describe '
                    f"{row['classification']} authority at row {row_number}"
                )

        key = (path, row['line_at_audit'], row['symbol'], row['kind'])
        if key in seen:
            failures.append(f'cache/job inventory: duplicate row {row_number}: {path!r}')
        seen.add(key)
    return failures, len(rows)


def object_coverage_inventory_issues(root: Path) -> tuple[list[str], int]:
    """Require representative route-to-DAO-to-test evidence for sensitive objects."""
    inventory_path = root / 'docs/tenant-object-coverage.tsv'
    if not inventory_path.is_file():
        return ['object coverage: missing docs/tenant-object-coverage.tsv'], 0
    with inventory_path.open(newline='') as handle:
        reader = csv.DictReader(handle, delimiter='\t')
        if reader.fieldnames != OBJECT_COVERAGE_INVENTORY_FIELDS:
            return ['object coverage: invalid header'], 0
        rows = list(reader)

    route_path = root / 'docs/tenant-route-inventory.tsv'
    if not route_path.is_file():
        return ['object coverage: route inventory is missing'], 0
    with route_path.open(newline='') as handle:
        route_rows = {
            row.get('existing_path', ''): row.get('resource_category', '')
            for row in csv.DictReader(handle, delimiter='\t')
        }

    failures: list[str] = []
    seen: set[str] = set()
    for row_number, row in enumerate(rows, start=2):
        if None in row or any(not row.get(field, '').strip() for field in OBJECT_COVERAGE_INVENTORY_FIELDS):
            failures.append(f'object coverage: incomplete row {row_number}')
            continue
        route = row['route'].strip()
        if route in seen:
            failures.append(f'object coverage: duplicate route at row {row_number}: {route!r}')
        seen.add(route)
        if route not in route_rows:
            failures.append(f'object coverage: route is not in route inventory at row {row_number}: {route!r}')
        elif row['resource_category'] != route_rows[route]:
            failures.append(f'object coverage: resource category mismatch at row {row_number}: {route!r}')
        for field, prefix, missing in (
            ('handler_source', 'crates/keycompute-server/src/handlers/', 'handler source'),
            ('dao_source', 'crates/', 'DAO source'),
            ('test_file', 'crates/integration-tests/tests/', 'test source'),
        ):
            path = row[field].strip()
            source = root / path
            if not path.startswith(prefix) or not source.is_file():
                failures.append(f'object coverage: missing {missing} at row {row_number}: {path!r}')
        dao_source = root / row['dao_source'].strip()
        handler_source = root / row['handler_source'].strip()
        if handler_source.is_file() and row['handler_symbol'].strip() not in handler_source.read_text():
            failures.append(f'object coverage: handler symbol is not present at row {row_number}: {row["handler_symbol"]!r}')
        if dao_source.is_file() and row['dao_symbol'].strip() not in dao_source.read_text():
            failures.append(f'object coverage: DAO symbol is not present at row {row_number}: {row["dao_symbol"]!r}')
        test_source = root / row['test_file'].strip()
        if test_source.is_file() and row['test_symbol'].strip() not in test_source.read_text():
            failures.append(f'object coverage: test symbol is not present at row {row_number}: {row["test_symbol"]!r}')
    covered_routes = set(seen)
    for family, matcher in OBJECT_COVERAGE_FAMILIES.items():
        inventory_routes = [route for route in route_rows if matcher.search(route)]
        if inventory_routes and not any(matcher.search(route) for route in covered_routes):
            failures.append(f'object coverage: family has no representative evidence: {family}')
    return failures, len(rows)


def cache_job_coverage_issues(root: Path) -> tuple[list[str], int]:
    """Require runtime test evidence for representative high-risk cache/job sites."""
    inventory_path = root / 'docs/tenant-cache-job-coverage.tsv'
    if not inventory_path.is_file():
        return ['cache/job coverage: missing docs/tenant-cache-job-coverage.tsv'], 0
    with inventory_path.open(newline='') as handle:
        reader = csv.DictReader(handle, delimiter='\t')
        if reader.fieldnames != CACHE_JOB_COVERAGE_FIELDS:
            return ['cache/job coverage: invalid header'], 0
        rows = list(reader)
    source_inventory = root / 'docs/tenant-cache-job-inventory.tsv'
    if not source_inventory.is_file():
        return ['cache/job coverage: source inventory is missing'], 0
    with source_inventory.open(newline='') as handle:
        inventory_keys = {(row.get('path', ''), row.get('symbol', '')) for row in csv.DictReader(handle, delimiter='\t')}

    failures: list[str] = []
    seen: set[tuple[str, str]] = set()
    for row_number, row in enumerate(rows, start=2):
        if None in row or any(not row.get(field, '').strip() for field in CACHE_JOB_COVERAGE_FIELDS):
            failures.append(f'cache/job coverage: incomplete row {row_number}')
            continue
        key = (row['path'].strip(), row['symbol'].strip())
        if key in seen:
            failures.append(f'cache/job coverage: duplicate site at row {row_number}: {key!r}')
        seen.add(key)
        if key not in inventory_keys:
            failures.append(f'cache/job coverage: site is not in source inventory at row {row_number}: {key!r}')
        source = root / key[0]
        test_source = root / row['test_file'].strip()
        if not source.is_file() or not key[0].startswith('crates/'):
            failures.append(f'cache/job coverage: missing source at row {row_number}: {key[0]!r}')
        elif key[1] not in source.read_text():
            failures.append(f'cache/job coverage: symbol is not present at row {row_number}: {key[1]!r}')
        if not test_source.is_file() or not row['test_file'].strip().startswith('crates/'):
            failures.append(f'cache/job coverage: missing test source at row {row_number}: {row["test_file"]!r}')
        elif row['test_symbol'].strip() not in test_source.read_text():
            failures.append(f'cache/job coverage: test symbol is not present at row {row_number}: {row["test_symbol"]!r}')
    return failures, len(rows)


def dynamic_sql_review_issues(root: Path) -> tuple[list[str], int]:
    """Track high-risk dynamic SQL builders with explicit predicate evidence."""
    inventory_path = root / 'docs/tenant-dynamic-sql-review.tsv'
    if not inventory_path.is_file():
        return ['dynamic SQL review: missing docs/tenant-dynamic-sql-review.tsv'], 0
    with inventory_path.open(newline='') as handle:
        reader = csv.DictReader(handle, delimiter='\t')
        if reader.fieldnames != DYNAMIC_SQL_REVIEW_FIELDS:
            return ['dynamic SQL review: invalid header'], 0
        rows = list(reader)
    failures: list[str] = []
    seen: set[tuple[str, str]] = set()
    for row_number, row in enumerate(rows, start=2):
        if None in row or any(not row.get(field, '').strip() for field in DYNAMIC_SQL_REVIEW_FIELDS):
            failures.append(f'dynamic SQL review: incomplete row {row_number}')
            continue
        key = (row['path'].strip(), row['symbol'].strip())
        if key in seen:
            failures.append(f'dynamic SQL review: duplicate site at row {row_number}: {key!r}')
        seen.add(key)
        source = root / key[0]
        test_source = root / row['test_file'].strip()
        if not source.is_file() or not key[0].startswith('crates/'):
            failures.append(f'dynamic SQL review: missing source at row {row_number}: {key[0]!r}')
        elif key[1] not in source.read_text():
            failures.append(f'dynamic SQL review: symbol is not present at row {row_number}: {key[1]!r}')
        elif any(token.strip() not in source.read_text() for token in row['required_tokens'].split('|')):
            failures.append(f'dynamic SQL review: required predicate token is missing at row {row_number}: {key!r}')
        if not test_source.is_file() or not row['test_file'].strip().startswith('crates/'):
            failures.append(f'dynamic SQL review: missing test source at row {row_number}: {row["test_file"]!r}')
        elif row['test_symbol'].strip() not in test_source.read_text():
            failures.append(f'dynamic SQL review: test symbol is not present at row {row_number}: {row["test_symbol"]!r}')
    return failures, len(rows)


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
            # Also reject retired ownership columns through arbitrary SQL aliases,
            # e.g. `FROM users u ... u.tenant_id`. The alias is extracted only
            # from a literal SQL relation, so prose and Rust identifiers do not
            # trigger this check.
            for index in range(len(sql) - 2):
                if sql[index].kind != 'identifier' or sql[index].value not in {'from', 'join'}:
                    continue
                if sql[index + 1].kind != 'identifier' or sql[index + 1].value != 'users':
                    continue
                alias_index = index + 2
                if alias_index >= len(sql):
                    continue
                if sql[alias_index].value == 'as':
                    alias_index += 1
                    if alias_index >= len(sql):
                        continue
                if sql[alias_index].value in {'where', 'join', 'left', 'right', 'inner', 'outer', 'on', 'cross'}:
                    continue
                alias = sql[alias_index].value
                if any(contains(code, [alias, '.', field]) for field in ('tenant_id', 'role')):
                    issues.append('retired users ownership reference through SQL alias')
    return sorted(set(issues))


def migration_runner_issues(source: str) -> list[str]:
    """Reject runtime compatibility DDL while allowing schema assertions."""
    tokens = tokenize(source, 'rust')
    issues = []
    for token in tokens:
        if token.kind != 'literal' or not re.search(r'\bALTER\s+TABLE\b', token.value, re.I):
            continue
        if token.value.strip().upper() == 'ALTER TABLE':
            # Existing schema tests use this exact value as a negative assertion.
            continue
        issues.append('runtime migration contains ALTER TABLE compatibility DDL')
    return sorted(set(issues))


def check_repository(root: Path) -> dict:
    files = subprocess.check_output(['git', '-C', str(root), 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], timeout=20).decode().split('\0')
    files = [name for name in files if name]
    failures, count = schema_issues(
        (root/'crates/keycompute-db/migrations/001_init.sql').read_text(),
        (root/'docs/tenant-schema-inventory.tsv').read_text(),
    )
    route_failures, route_count = route_inventory_issues(root)
    failures.extend(route_failures)
    cache_job_failures, cache_job_count = cache_job_inventory_issues(root, set(files))
    failures.extend(cache_job_failures)
    object_coverage_failures, object_coverage_count = object_coverage_inventory_issues(root)
    failures.extend(object_coverage_failures)
    cache_job_coverage_failures, cache_job_coverage_count = cache_job_coverage_issues(root)
    failures.extend(cache_job_coverage_failures)
    dynamic_sql_failures, dynamic_sql_count = dynamic_sql_review_issues(root)
    failures.extend(dynamic_sql_failures)
    release_gate_failures = release_gate_issues(root)
    failures.extend(release_gate_failures)
    migrations = {p.name for p in (root/'crates/keycompute-db/migrations').glob('*.sql')}
    if migrations != {'001_init.sql'}:
        failures.append('only the complete greenfield 001_init.sql may exist')
    rust_files = [name for name in files if name.endswith('.rs') and name.startswith(('crates/', 'packages/'))]
    for name in rust_files:
        try:
            if (root/name).is_symlink():
                failures.append(f'{name}: source symlink is unsupported by this guard')
                continue
            source = (root/name).read_text()
            failures.extend(f'{name}: {issue}' for issue in rust_issues(source))
            if name == 'crates/keycompute-db/src/migrations.rs':
                failures.extend(f'{name}: {issue}' for issue in migration_runner_issues(source))
        except ValueError:
            failures.append(f'{name}: source scanner could not establish a safe parse')
    return {'passed': not failures, 'schema_tables': count, 'route_literals_scanned': route_count,
            'cache_job_inventory_rows': cache_job_count,
            'object_coverage_rows': object_coverage_count,
            'cache_job_coverage_rows': cache_job_coverage_count,
            'dynamic_sql_review_rows': dynamic_sql_count,
            'release_gate': not release_gate_failures,
            'rust_files_scanned': len(rust_files), 'findings': failures,
            'scope': 'foundation/schema-inventory/route-inventory/object-coverage/cache-job-coverage/dynamic-sql-review/release-gate/cache-job-scope-contract/retired-symbols/source-boundary',
            'not_covered': ['arbitrary dynamically generated SQL',
                            'all object-level DAO predicates',
                            'frontend/browser acceptance', 'real backend browser acceptance',
                            'full production release and snapshot restore']}


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
