"""Deterministic protocol fixtures and bounded strict readers for the isolated lab.
Not real inference. Every response declares synthetic 8 input + 8 output tokens.
"""
from __future__ import annotations
import json
import uuid

PROTOCOLS=('chat','responses','anthropic','websocket','node')

def model_name(protocol):
    return 'claude-3-5-sonnet' if protocol=='anthropic' else 'gpt-4o'

def path_for(protocol):
    if protocol not in PROTOCOLS:raise ValueError('unsupported lab protocol')
    return '/v1/messages' if protocol=='anthropic' else '/v1/responses' if protocol in ('responses','websocket') else '/v1/chat/completions'

def request_body(protocol,stream,payload_bytes=0):
    if protocol not in PROTOCOLS or not 0<=payload_bytes<=8*1024*1024:raise ValueError('invalid fixture request')
    text='Hello '+('x'*payload_bytes);name=model_name(protocol)
    if protocol in ('responses','websocket'):
        body={'model':name,'input':text,'max_output_tokens':32,'stream':stream,'store':False}
        if protocol=='websocket':body.update(type='response.create',stream_id='lab_'+uuid.uuid4().hex)
        return body
    body={'model':('node:'+name) if protocol=='node' else name,'messages':[{'role':'user','content':text}],
          'max_tokens':32,'stream':stream}
    if protocol!='anthropic':body['stream_options']={'include_usage':True} if stream else None
    return body

def response_body(protocol,response_bytes=0,*,identifier=None,text=None):
    if not 0<=response_bytes<=8*1024*1024:raise ValueError('fixture response too large')
    text=text if text is not None else ('x'*response_bytes if response_bytes else 'Hello from isolated model')
    identifier=identifier or uuid.uuid4().hex
    if protocol=='anthropic':
        return {'id':'msg_'+identifier,'type':'message','role':'assistant','model':model_name(protocol),
                'content':[{'type':'text','text':text}],'stop_reason':'end_turn','stop_sequence':None,
                'usage':{'input_tokens':8,'output_tokens':8}}
    if protocol in ('responses','websocket'):
        return {'id':'resp_'+identifier,'object':'response','created_at':1,'status':'completed','error':None,
                'incomplete_details':None,'model':model_name(protocol),'output':[
                    {'id':'msg_'+identifier,'type':'message','role':'assistant','status':'completed',
                     'content':[{'type':'output_text','text':text,'annotations':[]}]}],
                'usage':{'input_tokens':8,'output_tokens':8,'total_tokens':16}}
    return {'id':'chatcmpl-'+identifier,'object':'chat.completion','created':1,'model':model_name(protocol),
            'choices':[{'index':0,'message':{'role':'assistant','content':text},'finish_reason':'stop'}],
            'usage':{'prompt_tokens':8,'completion_tokens':8,'total_tokens':16}}

def stream_events(protocol,response_bytes=0):
    """Yield (event-name, value, is-content) for a ten-chunk synthetic stream."""
    identifier=uuid.uuid4().hex
    pieces=['hello ']*10 if not response_bytes else ['x'*len(range(i,response_bytes,10)) for i in range(10)]
    final=response_body(protocol,response_bytes,identifier=identifier,text=''.join(pieces))
    if protocol=='anthropic':
        start=dict(final,content=[],stop_reason=None,usage={'input_tokens':8,'output_tokens':0})
        yield 'message_start',{'type':'message_start','message':start},False
        yield 'content_block_start',{'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}},False
        for text in pieces:yield 'content_block_delta',{'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':text}},True
        yield 'content_block_stop',{'type':'content_block_stop','index':0},False
        yield 'message_delta',{'type':'message_delta','delta':{'stop_reason':'end_turn','stop_sequence':None},'usage':{'output_tokens':8}},False
        yield 'message_stop',{'type':'message_stop'},False
    elif protocol in ('responses','websocket'):
        seq=0;in_progress=dict(final,status='in_progress',output=[],usage=None)
        yield 'response.created',{'type':'response.created','sequence_number':seq,'response':in_progress},False
        for text in pieces:
            seq+=1
            yield 'response.output_text.delta',{'type':'response.output_text.delta','sequence_number':seq,
                'item_id':'msg_'+identifier,'output_index':0,'content_index':0,'delta':text},True
        seq+=1
        yield 'response.completed',{'type':'response.completed','sequence_number':seq,'response':final},False
    else:
        for text in pieces:
            yield None,{'id':final['id'],'object':'chat.completion.chunk','created':1,'model':model_name(protocol),
                'choices':[{'index':0,'delta':{'content':text},'finish_reason':None}]},True
        yield None,{'id':final['id'],'object':'chat.completion.chunk','created':1,'model':model_name(protocol),
            'choices':[{'index':0,'delta':{},'finish_reason':'stop'}],'usage':final['usage']},False
        yield None,'[DONE]',False

def complete_json(value,protocol='chat',expected_bytes=0):
    if not isinstance(value,dict) or value.get('error') is not None:return False
    usage=value.get('usage')
    if expected_bytes:
        try:
            if protocol=='anthropic':text=''.join(c['text'] for c in value.get('content',[]) if c.get('type')=='text')
            elif protocol in ('responses','websocket'):text=''.join(c['text'] for item in value.get('output',[]) for c in item.get('content',[]) if c.get('type')=='output_text')
            else:text=value['choices'][0]['message']['content']
            if not isinstance(text,str) or len(text.encode())!=expected_bytes:return False
        except (KeyError,IndexError,TypeError,AttributeError):return False
    if not isinstance(usage,dict):return False
    if protocol=='anthropic':
        content=value.get('content')
        return value.get('type')=='message' and value.get('stop_reason')=='end_turn' and usage.get('input_tokens')==8 and usage.get('output_tokens')==8 and isinstance(content,list) and any(isinstance(c,dict) and c.get('type')=='text' and isinstance(c.get('text'),str) and c['text'] for c in content)
    if protocol in ('responses','websocket'):
        output=value.get('output')
        return value.get('object')=='response' and value.get('status')=='completed' and isinstance(value.get('id'),str) and value['id'].startswith('resp_') and usage.get('total_tokens')==16 and isinstance(output,list) and any(isinstance(item,dict) and isinstance(item.get('content'),list) and any(isinstance(c,dict) and c.get('type')=='output_text' and isinstance(c.get('text'),str) and c['text'] for c in item['content']) for item in output)
    choices=value.get('choices')
    if 'error' in value or usage.get('total_tokens')!=16 or not isinstance(choices,list) or not choices or not isinstance(choices[0],dict):return False
    message=choices[0].get('message')
    return isinstance(message,dict) and isinstance(message.get('content'),str) and bool(message['content'])

class Reader:
    def __init__(self,stream=True,protocol='chat',expected_bytes=0):
        self.stream=stream;self.protocol=protocol;self.content=False;self.usage=False;self.done=False;self.error=False;self.input_tokens=None;self.expected_bytes=expected_bytes;self.content_bytes=0
    def event(self,line):
        if not line.startswith(b'data:'):return
        data=line[5:].strip()
        if data==b'[DONE]':
            if self.protocol in ('chat','node'):self.done=True
            return
        try:self.accept(json.loads(data))
        except (ValueError,UnicodeError,TypeError):self.error=True
    def accept(self,value):
        if not isinstance(value,dict):self.error=True;return
        kind=value.get('type')
        if kind in ('error','response.failed','response.incomplete') or value.get('error') is not None:self.error=True;return
        if self.protocol=='anthropic':
            if kind=='message_start':
                message=value.get('message');usage=message.get('usage') if isinstance(message,dict) else None
                if not isinstance(usage,dict):self.error=True;return
                self.input_tokens=usage.get('input_tokens')
            elif kind=='content_block_delta':
                delta=value.get('delta')
                if not isinstance(delta,dict) or not isinstance(delta.get('text'),str):self.error=True;return
                self.content |= bool(delta['text']);self.content_bytes+=len(delta['text'].encode())
            elif kind=='message_delta':
                usage=value.get('usage');delta=value.get('delta')
                self.usage |= isinstance(usage,dict) and self.input_tokens==8 and usage.get('output_tokens')==8 and isinstance(delta,dict) and delta.get('stop_reason')=='end_turn'
            elif kind=='message_stop':self.done=True
        elif self.protocol in ('responses','websocket'):
            if kind=='response.output_text.delta':
                if not isinstance(value.get('delta'),str):self.error=True;return
                self.content |= bool(value['delta']);self.content_bytes+=len(value['delta'].encode())
            elif kind=='response.completed':
                valid=complete_json(value.get('response'),self.protocol,self.expected_bytes)
                self.done=valid;self.usage=valid;self.error |= not valid
        else:
            if 'error' in value:self.error=True;return
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
                if content:self.content_bytes+=len(content.encode())
    def complete(self):return bool(self.content and self.usage and self.done and not self.error and (not self.expected_bytes or self.content_bytes==self.expected_bytes))
