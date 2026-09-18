"""Separate-process deterministic model and bounded open-loop TLS client.
No external providers, implicit proxy, blind retries or unbounded client queue.
"""
from __future__ import annotations
import concurrent.futures
from collections import deque
import http.client
import http.server
import json
import os
from pathlib import Path
import ssl
import threading
import time
import uuid
from protocols import Reader, complete_json, path_for, request_body, response_body, stream_events
from connections import reusable_idle_connection

class Model(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    lock = threading.Lock()
    accepted = completed = disconnected = 0
    def log_message(self, *_): pass
    def do_GET(self):
        if self.path!='/stats':self.send_error(404);return
        with Model.lock:
            data={'accepted':Model.accepted,'completed':Model.completed,'disconnected':Model.disconnected}
        body=json.dumps(data).encode()
        self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        paths={'/v1/chat/completions':'chat','/v1/responses':'responses','/v1/messages':'anthropic'}
        protocol=paths.get(self.path)
        if protocol is None:self.send_error(404);return
        try:length=int(self.headers.get('Content-Length','0'))
        except ValueError:self.send_error(400);return
        if length<1 or length>80*1024*1024:
            self.send_error(413);self.close_connection=True;return
        try:request=json.loads(self.rfile.read(length))
        except (ValueError,UnicodeError):self.send_error(400);return
        if not isinstance(request,dict):self.send_error(400);return
        with Model.lock:Model.accepted+=1
        response_bytes=int(os.environ.get('KC_LAB_RESPONSE_BYTES','0'))
        try:
            if not request.get('stream'):
                body=json.dumps(response_body(protocol,response_bytes)).encode()
                self.send_response(200);self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);self.wfile.flush()
            else:
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Transfer-Encoding','chunked');self.end_headers()
                for event,data,content in stream_events(protocol,response_bytes):
                    if content:time.sleep(float(os.environ.get('KC_LAB_STREAM_MS','1000'))/10000)
                    self.chunk(data,event)
                self.wfile.write(b'0\r\n\r\n');self.wfile.flush()
            with Model.lock:Model.completed+=1
        except (BrokenPipeError,ConnectionResetError,TimeoutError):
            with Model.lock:Model.disconnected+=1
            self.close_connection=True
    def chunk(self,data,event=None):
        prefix=('event: '+event+'\n') if event else ''
        data=(prefix+'data: '+(data if isinstance(data,str) else json.dumps(data))+'\n\n').encode()
        self.wfile.write(f'{len(data):x}\r\n'.encode()+data+b'\r\n');self.wfile.flush()

class Server(http.server.ThreadingHTTPServer):
    daemon_threads=True
    request_queue_size=512

def model():
    threading.stack_size(256*1024)
    Server(('0.0.0.0',8080),Model).serve_forever()


def percentiles(values):
    if not values:return None
    values=sorted(values)
    return {name:values[min(len(values)-1,int((len(values)-1)*q))] for name,q in [('p50',.5),('p95',.95),('p99',.99),('max',1)]}

def json_read(host,path,token=None,tls=False):
    context=ssl.create_default_context(cafile='/lab/private/cert.pem') if tls else None
    conn=http.client.HTTPSConnection(host,443,context=context,timeout=10) if tls else http.client.HTTPConnection(host,3000 if host.startswith('gw') else 8080,timeout=10)
    try:
        conn.request('GET',path,headers={'Authorization':'Bearer '+token} if token else {})
        response=conn.getresponse()
        if response.status!=200:raise RuntimeError('diagnostic endpoint returned '+str(response.status))
        return json.loads(response.read(4*1024*1024))
    finally:conn.close()

thread_state=threading.local()
def request(key,body,stream,scheduled,timeout,protocol="chat",response_bytes=0):
    started=time.monotonic();sample={'status':0,'complete':False,'lag_ms':max(0,(started-scheduled)*1000)}
    conn=reusable_idle_connection(getattr(thread_state,'connection',None))
    if conn is None:
        conn=http.client.HTTPSConnection('nginx',443,context=ssl.create_default_context(cafile='/lab/private/cert.pem'),timeout=timeout)
        thread_state.connection=conn
    reader=Reader(stream,protocol,response_bytes);first=None;phase='send'
    try:
        headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'}
        if protocol=='anthropic':headers={'x-api-key':key,'anthropic-version':'2023-06-01','Content-Type':'application/json'}
        conn.request('POST',path_for(protocol),body=body,headers=headers)
        phase='headers'
        response=conn.getresponse();phase='body';sample['status']=response.status;sample['header_ms']=(time.monotonic()-started)*1000
        if stream and response.status==200:
            total=0
            while True:
                line=response.readline(16*1024*1024+1)
                if not line:break
                if len(line)>16*1024*1024 or not line.endswith(b'\n'):raise RuntimeError('incomplete or oversized SSE line')
                total+=len(line)
                if total>32*1024*1024:raise RuntimeError('response exceeded bounded reader')
                reader.event(line)
                if reader.content and first is None:first=(time.monotonic()-started)*1000
            sample['complete']=reader.complete()
        else:
            data=response.read(16*1024*1024+1)
            if len(data)>16*1024*1024:raise RuntimeError('response exceeded bounded reader')
            if response.status==200:
                sample['complete']=complete_json(json.loads(data),protocol,response_bytes)
                if sample['complete']:first=(time.monotonic()-started)*1000
    except (OSError,ValueError,http.client.HTTPException,RuntimeError) as error:
        sample['transport_error']=phase+':'+type(error).__name__
        conn.close();thread_state.connection=None;sample['complete']=False
    sample['first_content_ms']=first;sample['elapsed_ms']=(time.monotonic()-started)*1000
    return sample

def run_client():
    settings=json.loads(Path('/lab/private/settings.json').read_text());fixture=json.loads(Path('/lab/private/fixture.json').read_text())
    protocol=settings.get('protocol','chat')
    stream=settings['mode']=='sse';keys=fixture['keys'];timeout=max(30,settings['stream_ms']/1000+20)
    body=json.dumps(request_body(protocol,stream,settings.get('payload_bytes',0))).encode()
    warmups=[]
    for key in keys[:2]:warmups.append(request(key,body,stream,time.monotonic(),timeout,protocol,settings.get("response_bytes",0)))
    if not all(s['complete'] for s in warmups):raise RuntimeError('warmup failed')
    replicas=settings['replicas'];before=[json_read('gw'+str(i),'/api/v1/admin/monitoring/capacity',fixture['admin_token']) for i in range(replicas)]
    slots=threading.BoundedSemaphore(settings['client_workers']);samples=[];pending=set();drops=0;observations=deque(maxlen=600);observation_count=0
    stop=threading.Event()
    def monitor():
        nonlocal observation_count
        while not stop.is_set():
            observation_count+=1
            try:observations.append({'elapsed':time.monotonic()-started,'replicas':[json_read('gw'+str(i),'/api/v1/admin/monitoring/capacity',fixture['admin_token']) for i in range(replicas)]})
            except (OSError,ValueError,RuntimeError,http.client.HTTPException):observations.append({'diagnostics_failed':True})
            stop.wait(1)
    started=time.monotonic()
    Path('/lab/results/load-started').touch(exist_ok=False)
    monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
    planned=settings['seconds']*settings['rate']
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=settings['client_workers']) as executor:
            for n in range(planned):
                scheduled=started+n/settings['rate'];time.sleep(max(0,scheduled-time.monotonic()))
                if not slots.acquire(False):drops+=1;continue
                future=executor.submit(request,keys[n%len(keys)],body,stream,scheduled,timeout,protocol,settings.get("response_bytes",0))
                future.add_done_callback(lambda _:slots.release());pending.add(future)
                ready={f for f in pending if f.done()}
                samples.extend(f.result() for f in ready);pending-=ready
            samples.extend(f.result() for f in concurrent.futures.as_completed(pending))
        time.sleep(max(0,started+settings['seconds']-time.monotonic()))
        elapsed=time.monotonic()-started
    finally:stop.set();monitor_thread.join(12)
    good=[s for s in samples if s['status']==200 and s['complete']];statuses={};errors={}
    for sample in samples:
        statuses[str(sample['status'])]=statuses.get(str(sample['status']),0)+1
        if 'transport_error' in sample:
            name=sample['transport_error'];errors[name]=errors.get(name,0)+1
    report={'schema':'keycompute-separated-load-v1','settings':settings,'planned':planned,'sent':len(samples),
            'transport_error_counts':errors,'warmups':len(warmups),'completed_success':len(good),'generator_dropped':drops,'status_counts':statuses,
            'elapsed_seconds':elapsed,'successful_rps_including_tail':len(good)/elapsed,
            'header_ms':percentiles([s.get('header_ms',0) for s in good]),
            'complete_ms':percentiles([s['elapsed_ms'] for s in good]),'first_content_ms':percentiles([s['first_content_ms'] for s in good if s['first_content_ms'] is not None]),
            'schedule_lag_ms':percentiles([s['lag_ms'] for s in samples]),'before':before,'capacity_samples':list(observations),'capacity_samples_dropped':max(0,observation_count-len(observations)),
            'after':[json_read('gw'+str(i),'/api/v1/admin/monitoring/capacity',fixture['admin_token']) for i in range(replicas)],
            'model':json_read('node' if protocol=='node' else 'model','/stats'),
            'reader_scope':'Only complete valid responses are successful; header latency is not full response latency'}
    with open('/lab/results/client.json','x') as file:json.dump(report,file,indent=2)
    return 0 if len(good)/planned>=.99 and drops==0 else 2

if __name__=='__main__':
    import sys
    if sys.argv[1:] == ['model']:model()
    elif sys.argv[1:]==['client']:raise SystemExit(run_client())
    else:raise SystemExit('expected model or client')
