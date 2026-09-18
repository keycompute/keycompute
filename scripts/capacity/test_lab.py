"""Offline guards for the destructive boundary and response success accounting."""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import lab
from workload import Reader, complete_json, percentiles

DEFAULTS=dict(rate=20,seconds=30,tenants=4,users=2,accounts=8,mode='json',replicas=1,
              client_workers=128,stream_ms=1000,payload_bytes=0,writer_connections=10,
              global_limit=256,tenant_limit=32,account_limit=32,global_queue=128,
              tenant_queue=16,queue_ms=1000)

class SettingsTests(unittest.TestCase):
    def test_settings_are_bounded_before_resources_exist(self):
        self.assertEqual(lab.settings_from_args(SimpleNamespace(**DEFAULTS)),DEFAULTS)
        for key,value in [('rate',0),('rate',1001),('replicas',5),('seconds',3601),
                          ('payload_bytes',9*1024*1024),('writer_connections',1),
                          ('tenant_limit',300),('tenant_queue',129),('queue_ms',0)]:
            with self.subTest(key=key,value=value),self.assertRaises(ValueError):
                lab.settings_from_args(SimpleNamespace(**(DEFAULTS|{key:value})))
        with self.assertRaises(ValueError):
            lab.settings_from_args(SimpleNamespace(**(DEFAULTS|{'seconds':3600,'rate':1000})))
    def test_tls_addition_preserves_internal_health_and_bounded_upstreams(self):
        text='upstream x { server keycompute-server:3000; } server { listen       80; }'
        out=lab.nginx_config(text,2)
        for value in ['listen       80;', 'listen 443 ssl;', 'server gw0:3000;', 'server gw1:3000;']:
            self.assertIn(value,out)
        with self.assertRaises(ValueError):lab.nginx_config(out,2)
        with self.assertRaises(ValueError):lab.nginx_config(text,5)
    def test_output_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                lab.Lab(SimpleNamespace(**DEFAULTS,output=directory))
            destination=Path(directory)/'result';instance=lab.Lab(SimpleNamespace(**DEFAULTS,output=str(destination)))
            self.assertEqual(destination.stat().st_mode&0o777,0o700)
            lab.save(instance.results/'once.json',{'first':True})
            with self.assertRaises(FileExistsError):lab.save(instance.results/'once.json',{'first':False})
    def test_wrong_owner_blocks_exec_and_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            instance=lab.Lab(SimpleNamespace(**DEFAULTS,output=str(Path(directory)/'new')))
            instance.created=['unrelated'];instance.names={'postgres':'unrelated'}
            with patch('lab.run',return_value=SimpleNamespace(stdout=json.dumps([{'Config':{'Labels':{}}}]),returncode=0)) as call:
                with self.assertRaises(RuntimeError):instance.exec('postgres',['psql'])
                instance.close()
                self.assertTrue(instance.manifest['cleanup_errors'])
                self.assertTrue(all('rm' not in c.args[0] and 'exec' not in c.args[0] for c in call.call_args_list))

class ReaderTests(unittest.TestCase):
    def good(self):
        r=Reader(True);r.event(b'data: {"choices":[{"delta":{"content":"ok"}}]}\n')
        r.event(b'data: {"choices":[],"usage":{"total_tokens":16}}\n');r.event(b'data: [DONE]\n');return r
    def test_only_complete_valid_streams_succeed(self):
        self.assertTrue(self.good().complete())
        r=Reader(True);r.event(b'data: [DONE]\n');self.assertFalse(r.complete())
        for extra in [b'data: {"error":{}}',b'data: []',b'data: broken',b'data: {"usage":[]}',b'data: {"choices":[2]}',b'data: {"choices":[{"delta":3}]}']:
            r=self.good();r.event(extra);self.assertFalse(r.complete(),extra)
        r=self.good();r.done=False;self.assertFalse(r.complete())
        r=self.good();r.usage=False;self.assertFalse(r.complete())
    def test_json_shape_errors_are_not_success(self):
        self.assertTrue(complete_json({'choices':[{'message':{'content':'ok'}}],'usage':{'total_tokens':16}}))
        for value in [[],{}, {'choices':[1],'usage':[]}, {'choices':[], 'error':{}}, {'choices':[{'message':{'content':'ok'}}]}]:
            self.assertFalse(complete_json(value))
    def test_percentiles(self):
        self.assertIsNone(percentiles([]));self.assertEqual(percentiles([4,1,3,2])['max'],4)

if __name__=='__main__':unittest.main()
