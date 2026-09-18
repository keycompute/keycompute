"""Derive bounded admission settings for an EXPLICIT workload, without applying them.

The calculation uses an assumed mean holding time and load distribution. It is
not a capacity benchmark, queue-latency guarantee, or automatic deployment.
"""
from __future__ import annotations
import argparse
from decimal import Decimal, ROUND_CEILING
import json
from pathlib import Path


def ceil(value: Decimal) -> int:
    return int(value.to_integral_value(rounding=ROUND_CEILING))


def derive(*, rate: int, stream_ms: int, processing_ms: int = 250,
           tenants: int = 4, accounts: int = 8, replicas: int = 1,
           hottest_tenant_percent: int | None = None,
           hottest_instance_percent: int | None = None,
           headroom_percent: int = 20, global_limit: int = 256,
           queue_ms: int = 500, writer_connections: int = 10) -> dict:
    bounds = {'rate':(rate,1,1000), 'stream_ms':(stream_ms,0,60000),
              'processing_ms':(processing_ms,1,60000), 'tenants':(tenants,1,32),
              'accounts':(accounts,1,128), 'replicas':(replicas,1,4),
              'headroom_percent':(headroom_percent,0,200), 'global_limit':(global_limit,1,1024),
              'queue_ms':(queue_ms,1,60000), 'writer_connections':(writer_connections,2,64)}
    for name,(value,minimum,maximum) in bounds.items():
        if isinstance(value,bool) or not isinstance(value,int) or not minimum<=value<=maximum:
            raise ValueError(f'{name} must be an integer in {minimum}..{maximum}')
    def share(percent,count,name):
        minimum=Decimal(1)/count
        if percent is None:return minimum
        if isinstance(percent,bool) or not isinstance(percent,int) or not 1<=percent<=100:
            raise ValueError(f'{name} must be an integer percentage in 1..100')
        value=Decimal(percent)/100
        if value<minimum:raise ValueError(f'{name} cannot be below uniform share')
        return value
    instance_share=share(hottest_instance_percent,replicas,'hottest_instance_percent')
    tenant_share=share(hottest_tenant_percent,tenants,'hottest_tenant_percent')
    holding=Decimal(stream_ms+processing_ms)/1000
    reserve=Decimal(100+headroom_percent)/100
    cluster=Decimal(rate)*holding*reserve
    per_instance=ceil(cluster*instance_share)
    # No unmeasured independence assumption between tenant and instance skew:
    # one instance may receive the entire hot tenant, up to its total share.
    per_tenant=ceil(cluster*min(tenant_share,instance_share))
    per_account=ceil(cluster/Decimal(accounts))
    reasons=[]
    if per_instance>global_limit:reasons.append('assumed instance demand exceeds the explicit global budget')
    if per_tenant>global_limit:reasons.append('hot tenant demand exceeds the explicit instance budget')
    if per_account>global_limit:reasons.append('shared account demand exceeds the supported per-account setting')
    global_queue=ceil(Decimal(rate)*instance_share*Decimal(queue_ms)/1000)
    tenant_queue=min(global_queue,ceil(Decimal(rate)*min(tenant_share,instance_share)*Decimal(queue_ms)/1000))
    if global_queue>1024:reasons.append('derived queue exceeds the lab safety bound')
    config=None if reasons else {
        'global_limit':global_limit,'tenant_limit':max(1,per_tenant),'account_limit':max(1,per_account),
        'global_queue':global_queue,'tenant_queue':tenant_queue,'queue_timeout_ms':queue_ms}
    return {'schema':'keycompute-workload-plan-v1','feasible_by_arithmetic':not reasons,
        'assumptions':{'rate':rate,'stream_ms':stream_ms,'processing_ms':processing_ms,'tenants':tenants,
            'accounts':accounts,'replicas':replicas,'headroom_percent':headroom_percent,
            'hottest_tenant_share':str(tenant_share),'hottest_instance_share':str(instance_share),
            'upstream_account_distribution':'assumed balanced; authoritative account quotas still apply'},
        'required_with_headroom':{'cluster_inflight':ceil(cluster),'instance_inflight':per_instance,
            'hot_tenant_inflight_on_one_instance':per_tenant,'shared_account_inflight':per_account},
        'gateway_admission':config,'writer_connections_trial':writer_connections,'reasons':reasons,
        'limitations':['writer size is a trial input, never inferred from RPS',
            'queue size is a nominal arrival budget, not a waiting-time SLO',
            'holding time includes delivery and settlement, not just model duration',
            'headroom is a workload assumption, not a proven probability bound',
            'replica and account distribution must be measured; no linear scaling promise',
            'payload/RSS, downstream quotas and durable recovery need separate validation']}


def assess(client: dict, manifest: dict, *, first_content_p99_ms: float, complete_p99_ms: float) -> dict:
    """Add latency acceptance to the lab's success and accounting gates."""
    for value in (first_content_p99_ms,complete_p99_ms):
        if isinstance(value,bool) or not isinstance(value,(int,float)) or not 0<value<3600000:
            raise ValueError('finite positive latency targets are required')
    reasons=[]
    if not manifest.get('pass'):reasons.append('lab success/accounting/cleanup gate failed')
    if client.get('generator_dropped',1):reasons.append('load generator dropped work')
    planned=client.get('planned',0);success=client.get('completed_success',0)
    if not planned or success/planned<.99:reasons.append('complete success below 99%')
    for field,target in [('first_content_ms',first_content_p99_ms),('complete_ms',complete_p99_ms)]:
        p99=(client.get(field) or {}).get('p99')
        if not isinstance(p99,(float,int)) or not 0<=p99<=target:reasons.append(field+' p99 exceeds target or is unavailable')
    return {'accepted':not reasons,'reasons':reasons,
            'targets':{'first_content_p99_ms':first_content_p99_ms,'complete_p99_ms':complete_p99_ms}}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rate',type=int,required=True);parser.add_argument('--stream-ms',type=int,required=True)
    for name,default in [('processing-ms',250),('tenants',4),('accounts',8),('replicas',1),
                         ('headroom-percent',20),('global-limit',256),('queue-ms',500),('writer-connections',10)]:
        parser.add_argument('--'+name,type=int,default=default)
    parser.add_argument('--hottest-tenant-percent',type=int)
    parser.add_argument('--hottest-instance-percent',type=int)
    parser.add_argument('--report',type=Path)
    args=vars(parser.parse_args());report=args.pop('report');result=derive(**args)
    if report:
        with report.open('x') as output:json.dump(result,output,indent=2)
    print(json.dumps(result,indent=2))
    return 0 if result['feasible_by_arithmetic'] else 2

if __name__=='__main__':raise SystemExit(main())
