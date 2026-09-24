"""Bounded native execution-event contract, NOT the OpenSandbox wire protocol.

OpenSandbox's SSE adapter must translate its pinned event schema into this model.
Bytes remain bytes until a Harbor-specific decoding adapter applies its contract.
"""
from __future__ import annotations
from dataclasses import dataclass
import base64,hashlib,json

@dataclass(frozen=True)
class ExecResult:
    stdout:bytes
    stderr:bytes
    return_code:int

class ExecAccumulator:
    def __init__(self,run_id:str,generation:int,*,max_output_bytes:int=8*1024*1024,max_events:int=100000):
        if not isinstance(run_id,str) or not run_id:raise ValueError('invalid run identity')
        if type(generation) is not int or generation<1:raise ValueError('invalid generation')
        if type(max_output_bytes) is not int or max_output_bytes<1 or type(max_events) is not int or max_events<1:raise ValueError('invalid budget')
        self.run_id=run_id;self.generation=generation;self.max_output_bytes=max_output_bytes;self.max_events=max_events
        self._hashes={};self._stdout=bytearray();self._stderr=bytearray();self._return_code=None
    def accept(self,event:dict)->None:
        if not isinstance(event,dict):raise ValueError('event must be object')
        if event.get('run_id')!=self.run_id:raise ValueError('wrong run identity')
        if type(event.get('generation')) is not int or event['generation']!=self.generation:raise ValueError('stale generation')
        seq=event.get('sequence')
        if type(seq) is not int or seq<1:raise ValueError('invalid sequence')
        encoded=json.dumps(event,sort_keys=True,separators=(',',':'),allow_nan=False).encode()
        if len(encoded)>self.max_output_bytes*2+2048:raise ValueError('event budget exceeded')
        digest=hashlib.sha256(encoded).digest()
        if seq in self._hashes:
            if self._hashes[seq]!=digest:raise ValueError('replay payload mismatch')
            return
        if self._return_code is not None:raise ValueError('event after exit')
        if seq!=len(self._hashes)+1:raise ValueError('sequence gap')
        if len(self._hashes)>=self.max_events:raise ValueError('event budget exceeded')
        kind=event.get('kind')
        if kind=='output':
            stream=event.get('stream')
            if stream not in ('stdout','stderr') or not isinstance(event.get('data'),str):raise ValueError('invalid output')
            try:data=base64.b64decode(event['data'],validate=True)
            except (ValueError,TypeError) as exc:raise ValueError('invalid base64') from exc
            if len(self._stdout)+len(self._stderr)+len(data)>self.max_output_bytes:raise ValueError('output budget exceeded; full artifact transport required')
            (self._stdout if stream=='stdout' else self._stderr).extend(data)
        elif kind=='exit':
            code=event.get('return_code')
            if type(code) is not int or not 0<=code<=255:raise ValueError('invalid normalized exit code')
            self._return_code=code
        else:raise ValueError('unknown event kind')
        self._hashes[seq]=digest
    def result(self)->ExecResult:
        if self._return_code is None:raise ValueError('incomplete execution: no exit receipt')
        return ExecResult(bytes(self._stdout),bytes(self._stderr),self._return_code)
