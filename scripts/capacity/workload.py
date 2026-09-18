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
        if self.path!='/v1/chat/completions':self.send_error(404);return
        length=int(self.headers.get('Content-Length','0'))
        if length<1 or length>80*1024*1024:
            self.send_error(413);self.close_connection=True;return
        request=json.loads(self.rfile.read(length))
        with Model.lock:Model.accepted+=1
        response_id='chatcmpl-'+uuid.uuid4().hex
        usage={'prompt_tokens':8,'completion_tokens':8,'total_tokens':16}
        try:
            if not request.get('stream'):
                data={'id':response_id,'object':'chat.completion','created':1,'model':'gpt-4o',
                      'choices':[{'index':0,'message':{'role':'assistant','content':'Hello from isolated model'},'finish_reason':'stop'}],'usage':usage}
                body=json.dumps(data).encode();self.send_response(200);self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);self.wfile.flush()
            else:
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Transfer-Encoding','chunked');self.end_headers()
                for n in range(10):
                    time.sleep(float(os.environ.get('KC_LAB_STREAM_MS','1000'))/10000)
                    self.chunk({'id':response_id,'object':'chat.completion.chunk','created':1,'model':'gpt-4o',
                                'choices':[{'index':0,'delta':{'content':'hello '},'finish_reason':None}]})
                self.chunk({'id':response_id,'object':'chat.completion.chunk','created':1,'model':'gpt-4o',
                            'choices':[{'index':0,'delta':{},'finish_reason':'stop'}],'usage':usage})
                self.chunk('[DONE]');self.wfile.write(b'0\r\n\r\n');self.wfile.flush()
            with Model.lock:Model.completed+=1
        except (BrokenPipeError,ConnectionResetError,TimeoutError):
            with Model.lock:Model.disconnected+=1
            self.close_connection=True
    def chunk(self,data):
        data=('data: '+(data if isinstance(data,str) else json.dumps(data))+'\n\n').encode()
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

def complete_json(value):
    if not isinstance(value,dict) or 'error' in value:return False
    usage=value.get('usage');choices=value.get('choices')
    if not isinstance(usage,dict) or usage.get('total_tokens')!=16:return False
    if not isinstance(choices,list) or not choices or not isinstance(choices[0],dict):return False
    message=choices[0].get('message')
    return isinstance(message,dict) and isinstance(message.get('content'),str) and bool(message['content'])

class Reader:
    def __init__(self,stream):
        self.stream=stream;self.content=False;self.usage=False;self.done=False;self.error=False
    def event(self,line):
        if not line.startswith(b'data:'):return
        data=line[5:].strip()
        if data==b'[DONE]':self.done=True;return
        try:value=json.loads(data)
        except (ValueError,UnicodeError):self.error=True;return
        if not isinstance(value,dict):self.error=True;return
        self.error |= 'error' in value
        usage=value.get('usage')
        if usage is not None:
            if not isinstance(usage,dict):self.error=True;return
            self.usage |= usage.get('total_tokens')==16
        choices=value.get('choices',[])
        if not isinstance(choices,list):self.error=True;return
        if choices:
            if not isinstance(choices[0],dict):self.error=True;return
            delta=choices[0].get('delta',{})
            if not isinstance(delta,dict):self.error=True;return
            content=delta.get('content')
            if content is not None and not isinstance(content,str):self.error=True;return
            self.content |= bool(content)
    def complete(self):return self.content and self.usage and self.done and not self.error

thread_state=threading.local()
def request(key,body,stream,scheduled,timeout):
    started=time.monotonic();sample={'status':0,'complete':False,'lag_ms':max(0,(started-scheduled)*1000)}
    conn=getattr(thread_state,'connection',None)
    if conn is None:
        conn=http.client.HTTPSConnection('nginx',443,context=ssl.create_default_context(cafile='/lab/private/cert.pem'),timeout=timeout)
        thread_state.connection=conn
    reader=Reader(stream);first=None
    try:
        conn.request('POST','/v1/chat/completions',body=body,headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})
        response=conn.getresponse();sample['status']=response.status;sample['header_ms']=(time.monotonic()-started)*1000
        if stream and response.status==200:
            total=0
            while True:
                line=response.readline(1024*1024)
                if not line:break
                total+=len(line)
                if total>16*1024*1024:raise RuntimeError('response exceeded bounded reader')
                reader.event(line)
                if reader.content and first is None:first=(time.monotonic()-started)*1000
            sample['complete']=reader.complete()
        else:
            data=response.read(16*1024*1024+1)
            if len(data)>16*1024*1024:raise RuntimeError('response exceeded bounded reader')
            if response.status==200:
                sample['complete']=complete_json(json.loads(data))
                if sample['complete']:first=(time.monotonic()-started)*1000
    except (OSError,ValueError,http.client.HTTPException,RuntimeError):
        conn.close();thread_state.connection=None;sample['complete']=False
    sample['first_content_ms']=first;sample['elapsed_ms']=(time.monotonic()-started)*1000
    return sample

def run_client():
    settings=json.loads(Path('/lab/private/settings.json').read_text());fixture=json.loads(Path('/lab/private/fixture.json').read_text())
    stream=settings['mode']=='sse';keys=fixture['keys'];timeout=max(30,settings['stream_ms']/1000+20)
    body=json.dumps({'model':'gpt-4o','messages':[{'role':'user','content':'Hello '+('x'*settings.get('payload_bytes',0))}],
                     'max_tokens':32,'stream':stream,'stream_options':{'include_usage':True} if stream else None}).encode()
    warmups=[]
    for key in keys[:2]:warmups.append(request(key,body,stream,time.monotonic(),timeout))
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
    started=time.monotonic();monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
    planned=settings['seconds']*settings['rate']
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=settings['client_workers']) as executor:
            for n in range(planned):
                scheduled=started+n/settings['rate'];time.sleep(max(0,scheduled-time.monotonic()))
                if not slots.acquire(False):drops+=1;continue
                future=executor.submit(request,keys[n%len(keys)],body,stream,scheduled,timeout)
                future.add_done_callback(lambda _:slots.release());pending.add(future)
                ready={f for f in pending if f.done()}
                samples.extend(f.result() for f in ready);pending-=ready
            samples.extend(f.result() for f in concurrent.futures.as_completed(pending))
        time.sleep(max(0,started+settings['seconds']-time.monotonic()))
        elapsed=time.monotonic()-started
    finally:stop.set();monitor_thread.join(12)
    good=[s for s in samples if s['status']==200 and s['complete']];statuses={}
    for sample in samples:statuses[str(sample['status'])]=statuses.get(str(sample['status']),0)+1
    report={'schema':'keycompute-separated-load-v1','settings':settings,'planned':planned,'sent':len(samples),
            'warmups':len(warmups),'completed_success':len(good),'generator_dropped':drops,'status_counts':statuses,
            'elapsed_seconds':elapsed,'successful_rps_including_tail':len(good)/elapsed,
            'header_ms':percentiles([s.get('header_ms',0) for s in good]),
            'complete_ms':percentiles([s['elapsed_ms'] for s in good]),'first_content_ms':percentiles([s['first_content_ms'] for s in good if s['first_content_ms'] is not None]),
            'schedule_lag_ms':percentiles([s['lag_ms'] for s in samples]),'before':before,'capacity_samples':list(observations),'capacity_samples_dropped':max(0,observation_count-len(observations)),
            'after':[json_read('gw'+str(i),'/api/v1/admin/monitoring/capacity',fixture['admin_token']) for i in range(replicas)],
            'model':json_read('model','/stats'),
            'reader_scope':'Only complete valid responses are successful; header latency is not full response latency'}
    with open('/lab/results/client.json','x') as file:json.dump(report,file,indent=2)
    return 0 if len(good)/planned>=.99 and drops==0 else 2

if __name__=='__main__':
    import sys
    if sys.argv[1:] == ['model']:model()
    elif sys.argv[1:]==['client']:raise SystemExit(run_client())
    else:raise SystemExit('expected model or client')
