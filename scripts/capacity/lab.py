"""Disposable production-shaped capacity lab. No existing services are mutated.

Run from a clean source checkout, with explicitly supplied freshly built server
and fixture executables. Labels and random names prevent targeting production.
All data lives in fresh containers; no production credentials are accepted.
"""
from __future__ import annotations
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import threading
import time
import uuid
from postgres_probe import PostgresProbe, difference, check_owner
from protocols import PROTOCOLS

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

def run(args, *, timeout=30, check=True, input=None):
    return subprocess.run(args, input=input, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True, timeout=timeout, check=check)

def save(path, value):
    with path.open('x') as output:
        json.dump(value, output, indent=2)

def image_id(name):
    data=json.loads(run(['docker','image','inspect',name]).stdout)[0]
    return data['Id']

def bounded(value,minimum,maximum,name):
    if not minimum<=value<=maximum:raise ValueError(f'{name} must be {minimum}..{maximum}')

def settings_from_args(args):
    if not hasattr(args,'fault'):args.fault='none'
    if args.fault not in ('none','drain-rejoin'):raise ValueError('unsupported lab fault')
    if args.fault=='drain-rejoin' and (args.replicas<2 or args.seconds<20):raise ValueError('drain-rejoin requires >=2 disposable replicas and >=20 seconds')
    if not hasattr(args,'protocol'):args.protocol='chat'
    if not hasattr(args,'response_bytes'):args.response_bytes=0
    if args.protocol not in PROTOCOLS:raise ValueError('unsupported protocol')
    if args.protocol=='websocket' and not getattr(args,'websocket_bin',None):raise ValueError('websocket protocol requires a freshly built client binary')
    bounded(args.response_bytes,0,8*1024*1024,'response_bytes')
    for name,lo,hi in [('rate',1,1000),('seconds',1,3600),('tenants',1,32),('users',1,16),
                      ('accounts',1,128),('replicas',1,4),('client_workers',1,256),('stream_ms',10,60000),
                      ('payload_bytes',0,8*1024*1024),('writer_connections',2,64),
                      ('global_limit',1,1024),('tenant_limit',1,256),('account_limit',1,256),
                      ('global_queue',0,1024),('tenant_queue',0,256),('queue_ms',1,60000)]:
        bounded(getattr(args,name),lo,hi,name)
    if args.tenant_limit>args.global_limit or args.account_limit>args.global_limit or args.tenant_queue>args.global_queue:
        raise ValueError('scope limits exceed global limits')
    if args.rate*args.seconds>200000:raise ValueError('bounded lab request budget exceeded')
    return {k:getattr(args,k) for k in ('fault','protocol','response_bytes','rate','seconds','tenants','users','accounts','mode','replicas','client_workers',
        'stream_ms','payload_bytes','writer_connections','global_limit','tenant_limit','account_limit','global_queue','tenant_queue','queue_ms')}

def nginx_config(source,replicas):
    bounded(replicas,1,4,'replicas')
    if source.count('server keycompute-server:3000;')!=1 or source.count('listen       80;')!=1:
        raise ValueError('review changed Nginx upstream/listen layout before using this lab')
    source=source.replace('server keycompute-server:3000;', '\n        '.join('server gw'+str(i)+':3000;' for i in range(replicas)))
    # Retain the internal port 80 health check inherited from the web image.
    # Only 443 is published and the load client always verifies and uses TLS.
    return source.replace('listen       80;', 'listen       80;\n        listen 443 ssl;\n        ssl_certificate /cert/cert.pem;\n        ssl_certificate_key /cert/key.pem;',1)

def wait_for_load_start(path,stop,timeout):
    deadline=time.monotonic()+timeout
    while not path.is_file():
        if time.monotonic()>=deadline:raise TimeoutError('load client did not finish warmup')
        if stop.wait(.05):return False
    return not stop.is_set()

class Lab:
    def __init__(self,args):
        self.args=args;self.settings=settings_from_args(args);self.id=uuid.uuid4();self.tag=self.id.hex[:16]
        self.database='kc_load_'+self.id.hex;self.network='kc-capacity-'+self.tag
        self.out=Path(args.output).resolve()
        if self.out.exists():raise ValueError('output directory must be new; reports never overwrite')
        self.out.mkdir(parents=True,mode=0o700)
        self.private=self.out/'private';self.private.mkdir(mode=0o700)
        self.results=self.out/'results';self.results.mkdir(mode=0o700)
        self.logs=self.out/'logs';self.logs.mkdir(mode=0o700)
        self.created=[];self.network_created=False;self.images={};self.names={}
        self.stop=threading.Event();self.stats=[];self.activities=[];self.monitor=None;self.fault_thread=None;self.fault_error=None
        self.manifest={'run':str(self.id),'nonce':self.tag,'settings':self.settings,
            'scope':'isolated production server processes + TLS/Nginx + persistent Redis + PostgreSQL; local synthetic model',
            'limitations':['single host with unrelated host activity','local model is not real inference',
                'fixed operating point is not maximum capacity','self-signed lab CA, not production PKI'],
            'schema':'keycompute-capacity-lab-v1','state':'initializing','resources':{}}
        self.db_password=secrets.token_urlsafe(24)
        self.secret=secrets.token_urlsafe(40)
        self.crypto=base64.b64encode(secrets.token_bytes(32)).decode()

    def name(self,role):return 'kc-lab-'+self.tag+'-'+role
    def owned(self,kind,name):
        metadata=json.loads(run(['docker',kind,'inspect',name]).stdout)[0]
        labels=metadata.get('Labels',{}) if kind=='network' else metadata.get('Config',{}).get('Labels',{})
        if labels.get('keycompute.capacity_run')!=self.tag:raise RuntimeError('refusing action on an unowned resource')
        return metadata
    def container(self,role,image,command, *, env=None,cpu=1,memory='256m',mounts=(),ports=()):
        name=self.name(role)
        if run(['docker','container','inspect',name],check=False).returncode==0:raise RuntimeError('container name collision')
        args=['docker','create','--name',name,'--network',self.network,'--network-alias',role,
              '--label','keycompute.capacity_run='+self.tag,'--cpus',str(cpu),'--memory',memory,
              '--pids-limit','1024','--log-opt','max-size=20m','--log-opt','max-file=2']
        if env:
            env_path=self.private/(role+'.env')
            fd=os.open(env_path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
            with os.fdopen(fd,'w') as output:
                for key,value in env.items():
                    if '\n' in str(value):raise ValueError('multiline environment value')
                    output.write(key+'='+str(value)+'\n')
            args+=['--env-file',str(env_path)]
        for source,destination,read_only in mounts:
            source=Path(source).resolve()
            if ',' in str(source):raise ValueError('mount paths may not contain commas')
            args+=['--mount',f'type=bind,src={source},dst={destination}'+(',readonly' if read_only else '')]
        for port in ports:args+=['-p','127.0.0.1::'+str(port)]
        if role.startswith('gw') or role.startswith('fixture') or role=='websocket-client':
            args+=['--user','0:0','--workdir','/lab','--entrypoint',command[0]];command=command[1:]
        run(args+[image]+command)
        self.created.append(name);self.names[role]=name
        self.manifest['resources'][role]={'name':name,'image':image,'cpus':cpu,'memory':memory}
        run(['docker','start',name]);return name
    def exec(self,role,args,timeout=15,check=True):
        name=self.names[role];self.owned('container',name)
        return run(['docker','exec',name]+args,timeout=timeout,check=check)
    def wait(self,role,seconds=60):
        name=self.names[role];deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            info=self.owned('container',name)
            if not info['State']['Running']:return info['State']['ExitCode']
            time.sleep(.2)
        raise TimeoutError('lab container exceeded deadline: '+role)
    def startup(self):
        for key,name in [('runtime',self.args.runtime_image),('web',self.args.web_image),('python',self.args.python_image),('postgres','postgres:16-alpine'),('redis','redis:7-alpine')]:
            self.images[key]=image_id(name)
        self.manifest['images']=self.images
        self.manifest['git_commit']=run(['git','-C',str(ROOT),'rev-parse','HEAD']).stdout.strip()
        self.manifest['source_dirty']=bool(run(['git','-C',str(ROOT),'status','--porcelain']).stdout.strip())
        self.manifest['tracked_diff_sha256']=hashlib.sha256(run(['git','-C',str(ROOT),'diff','HEAD','--binary']).stdout.encode()).hexdigest()
        untracked=run(['git','-C',str(ROOT),'ls-files','--others','--exclude-standard','-z']).stdout.split('\0')
        self.manifest['untracked_hashes']={p:hashlib.sha256((ROOT/p).read_bytes()).hexdigest() for p in untracked if p and (ROOT/p).is_file()}
        binaries=[('server',self.args.server_bin),('fixture',self.args.fixture_bin)]
        if self.args.protocol=='websocket':binaries.append(('websocket',self.args.websocket_bin))
        for role,path in binaries:
            source=Path(path).resolve()
            if not source.is_file():raise ValueError('binary missing: '+role)
            self.manifest[role+'_sha256']=hashlib.sha256(source.read_bytes()).hexdigest()
            shutil.copyfile(source,self.private/role);(self.private/role).chmod(0o500)
        # Only the user explicitly supplied binaries are executed; no old server
        # executable from the runtime image is used.
        run(['docker','network','create','--internal','--label','keycompute.capacity_run='+self.tag,self.network]);self.network_created=True
        self.container('postgres',self.images['postgres'],['postgres','-c','shared_preload_libraries=pg_stat_statements',
            '-c','pg_stat_statements.track=all','-c','track_io_timing=on','-c','track_wal_io_timing=on'],
            env={'POSTGRES_PASSWORD':self.db_password,'POSTGRES_DB':self.database},cpu=1,memory='512m')
        self.container('redis',self.images['redis'],['redis-server','--maxmemory','256mb','--maxmemory-policy','noeviction',
            '--appendonly','yes','--appendfsync','everysec'],cpu=.5,memory='768m')
        self.container('cache',self.images['redis'],['redis-server','--maxmemory','128mb','--maxmemory-policy','allkeys-lru',
            '--save','','--appendonly','no'],cpu=.5,memory='256m')
        for _ in range(100):
            if self.exec('postgres',['pg_isready','-U','postgres','-d',self.database],check=False).returncode==0:break
            time.sleep(.1)
        else:raise TimeoutError('PostgreSQL did not start')
        self.exec('redis',['redis-cli','SET','keycompute:capacity:run',str(self.id)])
        self.container('model',self.images['python'],['python','/tool/workload.py','model'],
            env={'KC_LAB_STREAM_MS':str(self.args.stream_ms),'KC_LAB_RESPONSE_BYTES':str(self.args.response_bytes)},cpu=1,memory='256m',mounts=[(HERE,'/tool',True)])
        self.common={'DATABASE_URL':f'postgres://postgres:{self.db_password}@postgres:5432/{self.database}',
            'KC__DATABASE__URL':f'postgres://postgres:{self.db_password}@postgres:5432/{self.database}',
            'REDIS_URL':'redis://redis:6379','KC__REDIS__URL':'redis://redis:6379','KC__REDIS__CACHE_URL':'redis://cache:6379',
            'KC__AUTH__JWT_SECRET':self.secret,'KC__AUTH__JWT_ISSUER':'capacity-lab','KC__CRYPTO__SECRET_KEY':self.crypto,
            'KC__NODE_GATEWAY__REGISTRATION_TOKEN_SECRET':secrets.token_urlsafe(32),'KC__DEFAULT_ADMIN_PASSWORD':secrets.token_urlsafe(32),
            'KC__DEFAULT_ADMIN_EMAIL':'bootstrap-'+self.tag+'@example.invalid','APP_BASE_URL':'https://nginx',
            'KC__SERVER__BIND_ADDR':'0.0.0.0','KC__SERVER__PORT':'3000','RUST_LOG':'warn',
            'TOKIO_WORKER_THREADS':'2','KC__DATABASE__MAX_CONNECTIONS':str(self.args.writer_connections),
            'KC__GATEWAY__ADMISSION__GLOBAL_LIMIT':str(self.args.global_limit),
            'KC__GATEWAY__ADMISSION__TENANT_LIMIT':str(self.args.tenant_limit),
            'KC__GATEWAY__ADMISSION__ACCOUNT_LIMIT':str(self.args.account_limit),
            'KC__GATEWAY__ADMISSION__GLOBAL_QUEUE':str(self.args.global_queue),
            'KC__GATEWAY__ADMISSION__TENANT_QUEUE':str(self.args.tenant_queue),
            'KC__GATEWAY__ADMISSION__ACCOUNT_QUEUE':str(min(16,self.args.global_queue)),
            'KC__GATEWAY__ADMISSION__QUEUE_TIMEOUT_MS':str(self.args.queue_ms),
            'KC_LAB_ACK':'1','KC_LAB_RUN':str(self.id),'KC_LAB_FIXTURE':'/lab/private/fixture.json',
            'KC_LAB_PROTOCOL':self.args.protocol,'KC_LAB_TENANTS':str(self.args.tenants),'KC_LAB_USERS':str(self.args.users),'KC_LAB_ACCOUNTS':str(self.args.accounts)}
        self.container('fixture-seed',self.images['runtime'],['/lab/private/fixture','seed'],env=self.common,cpu=1,memory='512m',mounts=[(self.private,'/lab/private',False)])
        if self.wait('fixture-seed')!=0:raise RuntimeError('fixture seed failed; see private logs')
        # Schema initialization intentionally precedes extension installation.
        self.exec('postgres',['psql','-X','-U','postgres','-d',self.database,'-v','ON_ERROR_STOP=1','-c','CREATE EXTENSION IF NOT EXISTS pg_stat_statements;'])
        for i in range(self.args.replicas):
            env=self.common.copy();env['KC__DATABASE__URL']+='?application_name=capacity-gw'+str(i)
            self.container('gw'+str(i),self.images['runtime'],['/lab/private/server'],env=env,cpu=2,memory='1g',mounts=[(self.private/'server','/lab/private/server',True)])
            for _ in range(120):
                if self.exec('gw'+str(i),['curl','--max-time','1','--silent','--fail','http://127.0.0.1:3000/ready'],check=False).returncode==0:break
                if not self.owned('container',self.names['gw'+str(i)])['State']['Running']:raise RuntimeError('gateway exited before readiness')
                time.sleep(.5)
            else:raise TimeoutError('gateway did not become ready')
        self.certificate()
        config=nginx_config((ROOT/'nginx/nginx.conf').read_text(),self.args.replicas)
        (self.private/'nginx.conf').write_text(config)
        self.container('nginx',self.images['web'],['nginx','-g','daemon off;'],cpu=.5,memory='128m',mounts=[
            (self.private/'nginx.conf','/etc/nginx/nginx.conf',True),(self.private/'cert.pem','/cert/cert.pem',True),(self.private/'key.pem','/cert/key.pem',True)],ports=[443])
        self.exec('nginx',['nginx','-t'])
        (self.private/'settings.json').write_text(json.dumps(self.settings))
        if self.args.protocol=='node':
            self.container('node',self.images['python'],['python','/tool/node_worker.py'],cpu=1,memory='256m',mounts=[(HERE,'/tool',True),*[(self.private/('ca.pem' if n=='cert.pem' else n),'/lab/private/'+n,True) for n in ('fixture.json','settings.json','cert.pem')]])

        self.probe=PostgresProbe(self.names['postgres'],self.tag,self.database)
        self.manifest['actual_settings']=json.loads(self.exec('postgres',['psql','-X','-U','postgres','-d',self.database,'-At','-c',
            "SELECT json_build_object('fsync',current_setting('fsync'),'synchronous_commit',current_setting('synchronous_commit'),'version',current_setting('server_version'))"]).stdout)
        for role in ('redis','cache'):
            raw=self.exec(role,['redis-cli','INFO','persistence']).stdout
            persistence=dict(line.split(':',1) for line in raw.splitlines() if ':' in line)
            self.manifest['resources'][role]['persistence']={k:persistence.get(k) for k in ('aof_enabled','aof_last_write_status','rdb_last_bgsave_status')}
        if self.manifest['resources']['redis']['persistence']['aof_enabled']!='1':raise RuntimeError('critical AOF must be enabled')
        if self.manifest['actual_settings']['fsync']!='on' or self.manifest['actual_settings']['synchronous_commit']!='on':raise RuntimeError('durable PostgreSQL settings required')
        self.manifest['state']='ready'
    def certificate(self):
        # A real CA/leaf chain is required by strict Rustls clients; the server
        # certificate must not also be used as a CA. All material is lab-local.
        request_config=self.private/'request.cnf'
        request_config.write_text('[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=KeyCompute lab\n')
        run(['openssl','req','-config',str(request_config),'-x509','-newkey','rsa:2048','-nodes','-days','1',
            '-keyout',str(self.private/'ca-key.pem'),'-out',str(self.private/'ca.pem'),
            '-subj','/CN=KeyCompute disposable lab CA',
            '-addext','basicConstraints=critical,CA:TRUE',
            '-addext','keyUsage=critical,keyCertSign,cRLSign'])
        run(['openssl','req','-config',str(request_config),'-new','-newkey','rsa:2048','-nodes',
            '-keyout',str(self.private/'key.pem'),'-out',str(self.private/'server.csr'),'-subj','/CN=nginx'])
        extension=self.private/'server.ext'
        extension.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:nginx,DNS:localhost,IP:127.0.0.1\n')
        run(['openssl','x509','-req','-in',str(self.private/'server.csr'),'-CA',str(self.private/'ca.pem'),
            '-CAkey',str(self.private/'ca-key.pem'),'-set_serial','0x'+secrets.token_hex(8),
            '-days','1','-extfile',str(extension),'-out',str(self.private/'cert.pem')])
        for name in ('key.pem','ca-key.pem'):(self.private/name).chmod(0o600)
        run(['openssl','verify','-CAfile',str(self.private/'ca.pem'),str(self.private/'cert.pem')])
    def sample(self):
        start=time.monotonic()
        while not self.stop.is_set():
            try:
                row=self.probe.activity()
                self.activities.append({'elapsed':time.monotonic()-start,'groups':row})
                raw=run(['docker','stats','--no-stream','--format','{{json .}}']+[n for n in self.created if 'fixture' not in n],timeout=10,check=False)
                self.stats.append({'elapsed':time.monotonic()-start,'stats':[json.loads(line) for line in raw.stdout.splitlines() if line.startswith('{')]})
            except (ValueError,OSError,subprocess.SubprocessError):self.activities.append({'collection_failed':True})
            self.stop.wait(.5)
    def verify(self):
        self.container('fixture-verify',self.images['runtime'],['/lab/private/fixture','verify'],env=self.common,cpu=1,memory='256m',mounts=[(self.private,'/lab/private',True)])
        if self.wait('fixture-verify',20)!=0:raise RuntimeError('durable verification failed')
        raw=run(['docker','logs',self.names['fixture-verify']]).stdout
        return json.loads(raw.strip().splitlines()[-1])
    def drain_rejoin(self):
        """Only this run's replica may be drained. No production identifiers accepted."""
        events=[]
        try:
            warmup_budget=2*max(30,self.args.stream_ms/1000+20)+10*self.args.replicas+10
            if not wait_for_load_start(self.results/'load-started',self.stop,warmup_budget):return
            self.manifest['fault_after_load_start']=True
            if self.stop.wait(3):return
            gateway=self.names['gw0'];self.owned('container',gateway)
            original=(self.private/'nginx.conf').read_text()
            if original.count('server gw0:3000;')!=1:raise RuntimeError('unexpected lab upstream')
            # The exact mounted config is private to this lab. Existing streams
            # retain their old Nginx worker while new connections use survivors.
            (self.private/'nginx.conf').write_text(original.replace('server gw0:3000;',''))
            self.exec('nginx',['nginx','-t']);self.exec('nginx',['nginx','-s','reload'])
            events.append({'event':'removed_from_new_upstreams','at':time.time()})
            self.owned('container',gateway)
            run(['docker','stop','--time','130',gateway],timeout=140)
            stopped=self.owned('container',gateway)
            if stopped['State'].get('OOMKilled') or stopped['State']['ExitCode']!=0:raise RuntimeError('replica did not drain cleanly')
            events.append({'event':'drained','exit_code':stopped['State']['ExitCode'],'at':time.time()})
            self.owned('container',gateway);run(['docker','start',gateway])
            deadline=time.monotonic()+60
            while time.monotonic()<deadline:
                if self.exec('gw0',['curl','--max-time','1','--silent','--fail','http://127.0.0.1:3000/ready'],check=False).returncode==0:break
                if self.stop.wait(.25):raise RuntimeError('lab stopped during recovery')
            else:raise TimeoutError('restarted lab replica did not become ready')
            (self.private/'nginx.conf').write_text(original)
            self.exec('nginx',['nginx','-t']);self.exec('nginx',['nginx','-s','reload'])
            events.append({'event':'ready_and_rejoined','at':time.time()})
        except (OSError,ValueError,RuntimeError,subprocess.SubprocessError) as error:
            self.fault_error=type(error).__name__
        finally:self.manifest['recovery_events']=events

    def execute(self):
        before=self.probe.snapshot();save(self.results/'postgres-before.json',before)
        self.monitor=threading.Thread(target=self.sample,daemon=True);self.monitor.start()
        client_mounts=[(HERE,'/tool',True),*[(self.private/('ca.pem' if n=='cert.pem' else n),'/lab/private/'+n,True) for n in ('cert.pem','settings.json','fixture.json')],(self.results,'/lab/results',False)]
        client_role='client'
        if self.args.protocol=='websocket':
            client_role='websocket-client'
            self.container(client_role,self.images['runtime'],['/lab/private/websocket'],
                env={'KC_LAB_ACK':'1','KC_LAB_RUN':str(self.id)},cpu=2,memory='512m',
                mounts=client_mounts+[(self.private/'websocket','/lab/private/websocket',True)])
        else:
            self.container(client_role,self.images['python'],['python','/tool/workload.py','client'],cpu=2,memory='512m',mounts=client_mounts)
        if self.args.fault=='drain-rejoin':
            self.fault_thread=threading.Thread(target=self.drain_rejoin,daemon=True);self.fault_thread.start()
        status=self.wait(client_role,self.args.seconds+self.args.stream_ms/1000+120)
        if not (self.results/'client.json').exists():
            self.stop.set()
            raise RuntimeError('client failed without a report')
        if self.fault_thread:
            self.fault_thread.join(210)
            if self.fault_thread.is_alive() or self.fault_error:raise RuntimeError('disposable replica recovery failed')
        client=json.loads((self.results/'client.json').read_text());verified=None
        # Reuse ONLY this run's verifier container after the previous invocation
        # exited. It rechecks the private run marker and produces no mutations.
        for attempt in range(16):
            if attempt:
                self.owned('container',self.names['fixture-verify']);run(['docker','start',self.names['fixture-verify']])
                if self.wait('fixture-verify',20)!=0:raise RuntimeError('durable verification retry failed')
                verified=json.loads(run(['docker','logs',self.names['fixture-verify']]).stdout.strip().splitlines()[-1])
            else:verified=self.verify()
            if verified['active']==0 and verified['inconsistent']==0 and verified['invalid_usage']==0 and verified['ledger']==verified['settled']==client['model']['completed']:break
            time.sleep(2)
        after=self.probe.snapshot();save(self.results/'postgres-after.json',after)
        delta=difference(before,after);save(self.results/'postgres-delta.json',delta)
        self.manifest['postgres_interval_valid']=delta['valid_interval']
        self.manifest.update({'client_exit':status,'client_completed':client['completed_success'],'planned':client['planned'],
            'verification':verified,'model':client['model'],
            'pass':status==0 and verified['active']==0 and verified['inconsistent']==0 and verified['invalid_usage']==0 and verified['ledger']==verified['settled']==client['model']['completed'],
            'state':'completed'})
        return self.manifest['pass']
    def close(self):
        self.stop.set()
        if self.fault_thread:self.fault_thread.join(210)
        if self.monitor:self.monitor.join(25)
        save(self.results/'activity.json',self.activities);save(self.results/'container-samples.json',self.stats)
        for name in reversed(self.created):
            try:
                info=self.owned('container',name)
                # Logs are private evidence; application diagnostics may contain
                # identifiers. Do not include raw logs in public summaries.
                logs=run(['docker','logs','--tail','3000',name],check=False,timeout=10)
                (self.logs/(name+'.log')).write_text(logs.stdout+logs.stderr)
                self.manifest['resources'][next(k for k,v in self.names.items() if v==name)]['oom_killed']=info['State'].get('OOMKilled',False)
                run(['docker','rm','-f','-v',name],timeout=45)
            except (OSError,ValueError,subprocess.SubprocessError,RuntimeError) as error:
                self.manifest.setdefault('cleanup_errors',[]).append({'name':name,'type':type(error).__name__})
        if self.network_created:
            try:self.owned('network',self.network);run(['docker','network','rm',self.network])
            except (OSError,ValueError,subprocess.SubprocessError,RuntimeError):self.manifest.setdefault('cleanup_errors',[]).append({'network':self.network})
        if self.manifest.get('cleanup_errors') or any(v.get('oom_killed') for v in self.manifest['resources'].values()):self.manifest['pass']=False
        save(self.out/'manifest.json',self.manifest)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ack-isolated',action='store_true',required=True)
    parser.add_argument('--server-bin',required=True);parser.add_argument('--fixture-bin',required=True);parser.add_argument('--output',required=True)
    parser.add_argument('--runtime-image',default='keycompute-server:latest');parser.add_argument('--web-image',default='keycompute-web:latest');parser.add_argument('--python-image',default='python:3.12-alpine')
    parser.add_argument('--mode',choices=['json','sse'],default='json')
    parser.add_argument('--protocol',choices=PROTOCOLS,default='chat')
    parser.add_argument('--response-bytes',type=int,default=0)
    parser.add_argument('--websocket-bin')
    parser.add_argument('--fault',choices=['none','drain-rejoin'],default='none')
    for name,default in [('rate',20),('seconds',30),('tenants',4),('users',2),('accounts',8),('replicas',1),('client-workers',128),('stream-ms',1000),('payload-bytes',0),('writer-connections',10),('global-limit',256),('tenant-limit',32),('account-limit',32),('global-queue',128),('tenant-queue',16),('queue-ms',1000)]:
        parser.add_argument('--'+name,type=int,default=default)
    args=parser.parse_args();lab=Lab(args);passed=False
    try:lab.startup();passed=lab.execute()
    except (Exception,KeyboardInterrupt) as error:
        lab.manifest.update({'state':'failed','failure_type':type(error).__name__,'failure':str(error) if not isinstance(error,subprocess.SubprocessError) else 'child command failed; inspect private lab logs'})
    finally:lab.close()
    passed = passed and lab.manifest.get('pass',False)
    print(json.dumps({'output':str(lab.out),'pass':passed,'state':lab.manifest['state'],'cleanup_errors':lab.manifest.get('cleanup_errors',[])}))
    return 0 if passed and not lab.manifest.get('cleanup_errors') else 2

if __name__=='__main__':raise SystemExit(main())
