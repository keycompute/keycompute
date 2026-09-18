"""Read-only PostgreSQL 16 profiling for a nonce-labelled disposable lab.

Never reset global statistics or reconfigure a running database. Query text is
used only to classify statements, then removed (even normalized SQL can contain
credentials). WAL/fsync counters are cluster-wide; the lab must be exclusive.
"""
from __future__ import annotations
import json
import re
import subprocess
from typing import Any

SNAPSHOT = """
SELECT json_build_object(
 'database', current_database(), 'at', clock_timestamp(),
 'wal', (SELECT row_to_json(w) FROM pg_stat_wal w),
 'io_settings', json_build_object('track_io_timing', current_setting('track_io_timing'),
   'track_wal_io_timing',current_setting('track_wal_io_timing')),
 'statements_info',(SELECT row_to_json(i) FROM pg_stat_statements_info i),
 'statements',(SELECT coalesce(json_agg(s),'[]'::json) FROM (
   SELECT userid::text, queryid::text, toplevel, query, calls, total_exec_time, rows,
    shared_blks_hit,shared_blks_read,blk_read_time,blk_write_time,wal_records,wal_bytes
   FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database())
 ) s));
"""
ACTIVITY = """
SELECT coalesce(json_agg(a),'[]'::json) FROM (
 SELECT backend_type,state,wait_event_type,wait_event,count(*) AS connections
 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()
 GROUP BY backend_type,state,wait_event_type,wait_event
) a;
"""
TABLES = ('balance_reservations', 'balance_transactions', 'user_balances',
          'usage_logs', 'gateway_requests', 'gateway_request_attempts',
          'node_tasks', 'node_tips', 'response_affinities', 'accounts', 'tenants')

def check_owner(metadata: dict[str, Any], run_id: str) -> None:
    if not re.fullmatch(r'[a-f0-9]{12,32}', run_id):
        raise ValueError('invalid lab run ID')
    if metadata.get('Config', {}).get('Labels', {}).get('keycompute.capacity_run') != run_id:
        raise ValueError('container is not owned by this capacity run')

def classify(query: str) -> str:
    lowered = query.lower()
    if re.search(r'\bpg_(stat|settings|database|class|namespace)', lowered):
        return 'observer_or_catalog'
    tables = [t for t in TABLES if re.search(r'\b' + t + r'\b', lowered)]
    verb = re.match(r'\s*(\w+)', lowered)
    return (verb.group(1) if verb else 'unknown') + ':' + ','.join(tables or ['other'])

class PostgresProbe:
    def __init__(self, container: str, run_id: str, database: str):
        if not re.fullmatch(r'kc_load_[a-z0-9_]+', database):
            raise ValueError('profiling requires a disposable kc_load_ database')
        self.container, self.database = container, database
        meta = json.loads(subprocess.check_output(['docker', 'inspect', container], timeout=10))[0]
        check_owner(meta, run_id)

    def _query(self, query: str) -> Any:
        raw = subprocess.check_output(['docker', 'exec', self.container, 'psql', '-X', '-U',
            'postgres', '-d', self.database, '-v', 'ON_ERROR_STOP=1', '-At', '-c', query],
            timeout=10, stderr=subprocess.PIPE)
        return json.loads(raw)

    def snapshot(self) -> dict[str, Any]:
        data = self._query(SNAPSHOT)
        for statement in data['statements']:
            statement['class'] = classify(statement.pop('query'))
        return data

    def activity(self) -> list[dict[str, Any]]:
        return self._query(ACTIVITY)


def difference(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    """Keep reset/eviction uncertainty explicit; missing counters are NOT zero."""
    issues = []
    if before['database'] != after['database']:
        raise ValueError('database changed')
    for group in ('wal', 'statements_info'):
        if before[group]['stats_reset'] != after[group]['stats_reset']:
            issues.append(group + '_reset')
    if before['statements_info']['dealloc'] != after['statements_info']['dealloc']:
        issues.append('statement_entries_evicted')
    def numeric_delta(old, new):
        delta = {k: value - old.get(k, 0) for k, value in new.items()
                 if isinstance(value, (int, float)) and not isinstance(value, bool)}
        if any(v < 0 for v in delta.values()):
            issues.append('counter_decreased')
        return delta
    previous = {(s.get('userid'), s['queryid'], s['toplevel']): s for s in before['statements']}
    statements = []
    seen = set()
    for item in after['statements']:
        key = (item.get('userid'), item['queryid'], item['toplevel']); seen.add(key)
        delta = numeric_delta(previous.get(key, {}), item)
        if delta.get('calls', 0):
            statements.append({'userid': key[0], 'queryid': key[1], 'toplevel': key[2], 'class': item['class'], **delta})
    if set(previous) - seen:
        issues.append('statement_entries_disappeared')
    wal = numeric_delta(before['wal'], after['wal'])
    return {'database': before['database'], 'valid_interval': not issues,
            'limitations': sorted(set(issues)), 'wal_delta': wal,
            'statements_delta': sorted(statements, key=lambda s: s.get('total_exec_time', 0), reverse=True),
            'io_settings': after['io_settings']}
