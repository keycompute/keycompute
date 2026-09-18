"""Sequential, bounded protocol acceptance on labelled disposable capacity labs.
No production targets. Retain failures and stop rather than hiding them.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import subprocess
import sys
from lab import save

CASES = [
    ('chat-json', ['--rate','20','--seconds','20']),
    ('chat-large', ['--payload-bytes','2097152','--rate','1','--seconds','5']),
    ('responses-json', ['--protocol','responses','--rate','5','--seconds','10']),
    ('responses-sse', ['--protocol','responses','--mode','sse','--rate','5','--seconds','10']),
    ('messages-json', ['--protocol','anthropic','--rate','5','--seconds','10']),
    ('messages-sse', ['--protocol','anthropic','--mode','sse','--rate','5','--seconds','10']),
    ('messages-large', ['--protocol','anthropic','--payload-bytes','2097152','--rate','1','--seconds','5']),
    ('responses-large', ['--protocol','responses','--response-bytes','2097152','--rate','1','--seconds','5']),
    ('wss', ['--protocol','websocket','--mode','sse','--rate','8','--seconds','10','--stream-ms','500']),
    ('node-json', ['--protocol','node','--rate','3','--seconds','10']),
    ('node-sse', ['--protocol','node','--mode','sse','--rate','3','--seconds','10']),
    ('node-output', ['--protocol','node','--response-bytes','1572864','--rate','1','--seconds','5']),
    ('replicas-two', ['--replicas','2','--rate','20','--seconds','20']),
    ('drain-rejoin', ['--replicas','2','--protocol','responses','--mode','sse','--fault','drain-rejoin','--rate','8','--seconds','30','--stream-ms','1000']),
]

def summarize(name, status, directory):
    manifest=json.loads((directory/'manifest.json').read_text())
    report=directory/'results/client.json'
    client=json.loads(report.read_text()) if report.exists() else {}
    return {'case':name,'exit':status,'pass':status==0 and manifest.get('pass',False),
        'verification':manifest.get('verification'),'failure':manifest.get('failure'),
        'cleanup_errors':manifest.get('cleanup_errors',[]),
        'recovery_events':manifest.get('recovery_events',[]),
        **{key:client.get(key) for key in ('settings','planned','completed_success',
            'generator_dropped','status_counts','successful_rps_including_tail',
            'complete_ms','first_content_ms','schedule_lag_ms')}}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ack-isolated',action='store_true',required=True)
    for name in ('server-bin','fixture-bin','websocket-bin','output'):
        parser.add_argument('--'+name,required=True)
    parser.add_argument('--case',choices=[name for name,_ in CASES])
    args=parser.parse_args();output=Path(args.output).resolve()
    if output.exists():raise ValueError('output directory must not already exist')
    output.mkdir(parents=True,mode=0o700)
    executable=Path(__file__).resolve().with_name('lab.py')
    results=[]
    selected=[case for case in CASES if args.case is None or case[0]==args.case]
    for name,settings in selected:
        command=[sys.executable,str(executable),'--ack-isolated',
            '--server-bin',str(Path(args.server_bin).resolve()),
            '--fixture-bin',str(Path(args.fixture_bin).resolve()),
            '--websocket-bin',str(Path(args.websocket_bin).resolve()),
            '--output',str(output/name),'--stream-ms','100']+settings
        print('RUN '+name,flush=True)
        with (output/(name+'.log')).open('x') as log:
            try:
                result=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,timeout=300)
                status=result.returncode
            except subprocess.TimeoutExpired:
                # Do not claim the lab cleaned up after the driver was killed.
                results.append({'case':name,'pass':False,'timeout':True,
                    'cleanup_unverified':True})
                save(output/'summary.json',results)
                return 2
        value=summarize(name,status,output/name);results.append(value)
        save(output/(name+'-summary.json'),value)
        print(json.dumps(value),flush=True)
        if not value['pass']:
            save(output/'summary.json',results)
            return 2
    save(output/'summary.json',results)
    return 0

if __name__=='__main__':raise SystemExit(main())
