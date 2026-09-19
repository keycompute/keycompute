"""Optional, bounded console read mix for the isolated TLS capacity fixture.
No writes, implicit proxy, retry, or external endpoint is used.
"""
from __future__ import annotations
import concurrent.futures
import http.client
import json
import ssl
import time

PATHS = ('/api/v1/dashboard/overview', '/api/v1/usage/trend',
         '/api/v1/usage/stats', '/api/v1/payments/balance')

def valid_body(endpoint, value):
    """Validate read-only response shapes before counting HTTP 200 as success."""
    if not isinstance(value, dict) or 'error' in value: return False
    if endpoint.endswith('/balance'): return isinstance(value.get('available_balance'), str)
    if endpoint.endswith('/stats'): return type(value.get('total_requests')) is int
    if endpoint.endswith('/trend'):
        buckets = value.get('buckets')
        return isinstance(buckets, list) and all(isinstance(b,dict) and type(b.get('requests')) is int for b in buckets)
    if endpoint.endswith('/overview'):
        stats = value.get('stats')
        return isinstance(stats,dict) and type(stats.get('total_requests')) is int and all(isinstance(value.get(k),list) for k in ('active_keys','recent_usage','recent_orders'))
    return False

def read_one(token, path, scheduled):
    started = time.monotonic()
    sample = {'path': path, 'status': 0, 'complete': False,
              'lag_ms': max(0, (started - scheduled) * 1000)}
    conn = http.client.HTTPSConnection('nginx', 443, timeout=5,
        context=ssl.create_default_context(cafile='/lab/private/cert.pem'))
    try:
        conn.request('GET', path, headers={'Authorization': 'Bearer ' + token})
        response = conn.getresponse(); sample['status'] = response.status
        body = response.read(1024 * 1024 + 1)
        if len(body) > 1024 * 1024: raise ValueError('oversized response')
        value = json.loads(body)
        sample['complete'] = response.status == 200 and valid_body(path, value)
    except (OSError, ValueError, http.client.HTTPException) as error:
        sample['error'] = type(error).__name__
    finally: conn.close()
    sample['elapsed_ms'] = (time.monotonic() - started) * 1000
    return sample

def run(token, rate, seconds, started, report):
    if not rate: return
    planned = rate * seconds; samples = []; pending = set(); dropped = 0
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
            for n in range(planned):
                scheduled = started + n / rate
                time.sleep(max(0, scheduled - time.monotonic()))
                ready = {f for f in pending if f.done()}
                samples.extend(f.result() for f in ready); pending -= ready
                if len(pending) >= 16:
                    dropped += 1; continue
                pending.add(executor.submit(read_one, token, PATHS[n % len(PATHS)], scheduled))
            samples.extend(f.result() for f in concurrent.futures.as_completed(pending))
        counts = {}; elapsed = []
        for sample in samples:
            status = str(sample['status']); counts[status] = counts.get(status, 0) + 1
            if sample['complete']: elapsed.append(sample['elapsed_ms'])
        elapsed.sort()
        latency = {name: elapsed[min(len(elapsed)-1, int((len(elapsed)-1)*q))]
                   for name,q in [('p50',.5),('p95',.95),('p99',.99)]} if elapsed else None
        report.update({'planned': planned, 'sent': len(samples), 'dropped': dropped,
                       'status_counts': counts, 'complete_ms': latency,
                       'transport_errors': sum('error' in s for s in samples),
                       'invalid_successes': sum(s['status']==200 and not s['complete'] for s in samples),
                       'scope': 'one isolated admin fixture, four read endpoints, no write/retry'})
    except Exception as error:
        report['harness_error'] = type(error).__name__
