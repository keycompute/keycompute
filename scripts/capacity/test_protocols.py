import json
import re
import unittest
from pathlib import Path
from protocols import PROTOCOLS, Reader, complete_json, request_body, response_body, stream_events, path_for

class ProtocolTests(unittest.TestCase):
    def test_fixture_round_trip_each_protocol(self):
        for protocol in PROTOCOLS:
            with self.subTest(protocol=protocol):
                value=response_body(protocol)
                self.assertTrue(complete_json(value,protocol))
                reader=Reader(True,protocol)
                for _,event,_ in stream_events(protocol):
                    reader.event(('data: '+(event if isinstance(event,str) else json.dumps(event))+'\n').encode())
                self.assertTrue(reader.complete())
    def test_declared_large_output_is_actually_received(self):
        for protocol in PROTOCOLS:
            value=response_body(protocol,4096)
            self.assertTrue(complete_json(value,protocol,4096))
            self.assertFalse(complete_json(value,protocol,4097))
            reader=Reader(True,protocol,4096)
            for _,event,_ in stream_events(protocol,4096):
                reader.event(('data: '+(event if isinstance(event,str) else json.dumps(event))+'\n').encode())
            self.assertTrue(reader.complete(),protocol)
            reader.expected_bytes=4097;self.assertFalse(reader.complete())
    def test_error_and_truncation_are_not_success(self):
        for protocol in PROTOCOLS:
            events=list(stream_events(protocol))
            reader=Reader(True,protocol)
            for _,event,_ in events[:-1]:reader.event(('data: '+json.dumps(event)+'\n').encode())
            self.assertFalse(reader.complete(),protocol)
            for _,event,_ in events:reader.event(('data: '+(event if isinstance(event,str) else json.dumps(event))+'\n').encode())
            reader.event(b'data: {"type":"error","error":{}}\n')
            self.assertFalse(reader.complete(),protocol)
            self.assertFalse(complete_json({'error':{'message':'failure'}},protocol))
    def test_json_negative_shapes_and_usage(self):
        for protocol in PROTOCOLS:
            for value in [None,[],{},42,'bad']:
                self.assertFalse(complete_json(value,protocol))
            value=response_body(protocol);value['usage']={};self.assertFalse(complete_json(value,protocol))
            value=response_body(protocol);value['error']={'message':'failure'};self.assertFalse(complete_json(value,protocol))
    def test_bounded_payloads_and_entry_paths(self):
        for protocol in PROTOCOLS:
            self.assertIn(path_for(protocol),['/v1/chat/completions','/v1/messages','/v1/responses'])
            body=request_body(protocol,False,20)
            self.assertIn('x'*20,json.dumps(body))
            with self.assertRaises(ValueError):request_body(protocol,False,9*1024*1024)
        with self.assertRaises(ValueError):path_for('unsupported')
        with self.assertRaises(ValueError):response_body('chat',-1)
    def test_nginx_has_protocol_specific_limits_without_unbounded_defaults(self):
        config=(Path(__file__).resolve().parents[2]/'nginx/nginx.conf').read_text()
        for path,limit in [('/v1/chat/completions','96m'),('/v1/messages','32m'),('/v1/responses','80m'),('/node/v1/','2m')]:
            match=re.search(r'location\s+(?:\^~\s+)?'+re.escape(path)+r'\s*\{([^}]*)\}',config)
            self.assertIsNotNone(match,path)
            self.assertRegex(match[1],r'client_max_body_size\s+'+limit+r';')
            if path!='/v1/responses':self.assertIn('proxy_request_buffering off;',match[1])
        self.assertNotRegex(config,r'client_max_body_size\s+0\s*;')

if __name__=='__main__':unittest.main()
