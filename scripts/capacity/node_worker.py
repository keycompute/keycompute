"""Disposable session-authenticated Node worker. Never registers a production node.
Fixture creation is explicit; poll/heartbeat/completion use real HTTP handlers.
"""
import http.client
import json
from pathlib import Path
import ssl
import threading
import time
from workload import Model, Server
from protocols import response_body

ROOT=Path('/lab/private')
def post(path,body,token,timeout=40):
    conn=http.client.HTTPSConnection('nginx',443,context=ssl.create_default_context(cafile=str(ROOT/'cert.pem')),timeout=timeout)
    try:
        conn.request('POST',path,body=json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        response=conn.getresponse();data=response.read(8*1024*1024+1)
        if len(data)>8*1024*1024 or response.status!=200:raise RuntimeError('Node control request failed')
        value=json.loads(data)
        if not isinstance(value,dict):raise RuntimeError('Node control response shape')
        return value
    finally:conn.close()

def main():
    fixture=json.loads((ROOT/'fixture.json').read_text());settings=json.loads((ROOT/'settings.json').read_text());node=fixture['node']
    if settings.get('protocol')!='node' or not node:raise RuntimeError('explicit Node fixture required')
    identity={'protocol_version':'node.v1','node_id':node['node_id'],'session_id':node['session_id']};token=node['session_token']
    def heartbeat():
        while True:
            try:post('/node/v1/heartbeat',dict(identity,accepted_models=[node['model']]),token,10)
            except (OSError,ValueError,RuntimeError,http.client.HTTPException):pass
            time.sleep(2)
    def work():
        while True:
            try:
                result=post('/node/v1/tasks/poll',identity,token)
                task=result.get('task')
                if not task:time.sleep(min(5,max(.1,result.get('retry_after_ms',1000)/1000)));continue
                with Model.lock:Model.accepted+=1
                time.sleep(min(1,settings['stream_ms']/1000))
                completion=dict(identity,task_id=task['task_id'],lease_id=task['lease_id'],result={'status':'succeeded','response':response_body('node',settings.get('response_bytes',0))})
                answer=post('/node/v1/tasks/'+task['task_id']+'/complete',completion,token)
                if answer.get('action')!='succeeded':raise RuntimeError('Node completion not accepted')
                with Model.lock:Model.completed+=1
            except (OSError,ValueError,KeyError,RuntimeError,http.client.HTTPException):
                # Never blindly repeat a possibly committed completion.
                with Model.lock:Model.disconnected+=1
                time.sleep(.2)
    threading.Thread(target=heartbeat,daemon=True).start()
    for _ in range(4):threading.Thread(target=work,daemon=True).start()
    Server(('0.0.0.0',8080),Model).serve_forever()

if __name__=='__main__':main()
