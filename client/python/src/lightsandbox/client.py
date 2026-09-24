"""Executable native HTTP client for the planned v1 protocol.

No daemon is included. Tests use HTTPX's MockTransport and prove client behavior,
not isolation, runtime compatibility or command execution. No request is retried
implicitly, including when an idempotency key is present.
"""
from __future__ import annotations
import asyncio,time
from urllib.parse import urlsplit
import httpx
from .contracts import SandboxRef, ExecSpec, identifier, positive_int, text

class ApiError(RuntimeError):
    def __init__(self,code:str,message:str,*,request_id:str|None=None,status_code:int|None=None):
        super().__init__(message);self.code=code;self.request_id=request_id;self.status_code=status_code
class AmbiguousRequest(ApiError):
    def __init__(self,key:str,method:str,path:str):
        super().__init__('REQUEST_OUTCOME_UNKNOWN',f'{method} {path}: response lost; query original request before resubmission')
        self.idempotency_key=key;self.method=method;self.path=path
class WaitTimeout(ApiError):
    def __init__(self,resource_id:str):
        super().__init__('WAIT_TIMEOUT','client wait deadline elapsed; server operation was not cancelled')
        self.resource_id=resource_id
class OperationUnknown(ApiError):
    def __init__(self,resource_id:str):
        super().__init__('OPERATION_AMBIGUOUS','operation requires server-side reconciliation')
        self.resource_id=resource_id

def _connection(endpoint,token,transport,asynchronous):
    p=urlsplit(endpoint)
    if p.username or p.password or p.query or p.fragment:raise ValueError('invalid endpoint components')
    if p.scheme=='unix':
        if p.netloc or not p.path.startswith('/') or not p.path or '\0' in p.path:raise ValueError('invalid UNIX socket')
        cls=httpx.AsyncHTTPTransport if asynchronous else httpx.HTTPTransport
        transport=transport or cls(uds=p.path,retries=0)
        url='http://lightsandbox.local'
    else:
        if p.scheme not in ('https','http') or not p.hostname or p.path not in ('','/'):raise ValueError('endpoint must be an HTTP(S) origin or unix:///path')
        if p.scheme=='http' and p.hostname not in ('localhost','127.0.0.1','::1'):
            raise ValueError('remote cleartext transport is disabled')
        url=endpoint.rstrip('/')
    headers={'Accept':'application/json','User-Agent':'lightsandbox-reference/0.5.0'}
    if token:
        text(token,'token')
        if '\r' in token or '\n' in token:raise ValueError('invalid token')
        headers['Authorization']='Bearer '+token
    return dict(base_url=url,transport=transport,headers=headers,timeout=30.0,trust_env=False,follow_redirects=False)

def _headers(key=None,ref=None):
    h={}
    if key is not None:
        text(key,'idempotency key')
        if len(key)>256 or '\r' in key or '\n' in key:raise ValueError('invalid idempotency key')
        h['Idempotency-Key']=key
    if ref is not None:
        if not isinstance(ref,SandboxRef):raise ValueError('SandboxRef required')
        h['X-LS-Generation']=str(ref.generation)
    return h

def _decode(r:httpx.Response)->dict:
    if not 200<=r.status_code<300:
        try:e=r.json().get('error',{});e=e if isinstance(e,dict) else {}
        except (ValueError,AttributeError):e={}
        raise ApiError(e.get('code','HTTP_ERROR'),e.get('message',f'HTTP {r.status_code}'),request_id=e.get('request_id'),status_code=r.status_code)
    if r.status_code==204:return {}
    try:value=r.json()
    except ValueError as exc:raise ApiError('PROTOCOL_ERROR','expected a JSON response') from exc
    if not isinstance(value,dict):raise ApiError('PROTOCOL_ERROR','expected a JSON object')
    return value

def _poll_result(item,resource_id):
    state=item.get('status')
    if state=='succeeded':return item
    if state=='failed':
        error=item.get('error')
        if not isinstance(error,dict):raise ApiError('PROTOCOL_ERROR','operation error must be an object')
        raise ApiError(error.get('code','OPERATION_FAILED'),error.get('message','operation failed'))
    if state=='unknown':raise OperationUnknown(resource_id)
    if state not in ('pending','running'):raise ApiError('PROTOCOL_ERROR','unrecognized operation status')
    return None

class _Sandboxes:
    def __init__(self,c):self._c=c
    def create(self,spec:dict,*,idempotency_key:str):
        if not isinstance(spec,dict):raise ValueError('spec must be object')
        return self._c._send('POST','/v1/sandboxes',body=spec,key=idempotency_key)
    def get(self,ref:SandboxRef):return self._c._send('GET',f'/v1/sandboxes/{ref.sandbox_id}',ref=ref)
    def delete(self,ref:SandboxRef,*,idempotency_key:str):return self._c._send('DELETE',f'/v1/sandboxes/{ref.sandbox_id}',key=idempotency_key,ref=ref)

class _Executions:
    def __init__(self,c):self._c=c
    def start(self,ref:SandboxRef,spec:ExecSpec,*,idempotency_key:str):
        if not isinstance(spec,ExecSpec):raise ValueError('ExecSpec required')
        return self._c._send('POST',f'/v1/sandboxes/{ref.sandbox_id}/executions',body=spec.to_dict(),ref=ref,key=idempotency_key)
    def get(self,execution_id:str):return self._c._send('GET',f'/v1/executions/{identifier(execution_id)}')
    def cancel(self,execution_id:str,*,scope:str,idempotency_key:str):
        if scope not in ('process','tree','sandbox'):raise ValueError('invalid cancel scope')
        return self._c._send('POST',f'/v1/executions/{identifier(execution_id)}:cancel',body={'scope':scope},key=idempotency_key)

class _Operations:
    def __init__(self,c):self._c=c
    def get(self,operation_id:str):return self._c._send('GET',f'/v1/operations/{identifier(operation_id)}')
    def wait(self,operation_id:str,*,timeout_ms:int=30000,poll_ms:int=100):
        positive_int(timeout_ms,'timeout_ms');positive_int(poll_ms,'poll_ms')
        end=time.monotonic()+timeout_ms/1000
        while True:
            remaining=end-time.monotonic()
            if remaining<=0:raise WaitTimeout(operation_id)
            item=self._c._send('GET',f'/v1/operations/{identifier(operation_id)}',request_timeout=remaining)
            result=_poll_result(item,operation_id)
            if result is not None:return result
            time.sleep(min(poll_ms/1000,max(0,end-time.monotonic())))

class _AsyncOperations(_Operations):
    async def wait(self,operation_id:str,*,timeout_ms:int=30000,poll_ms:int=100):
        positive_int(timeout_ms,'timeout_ms');positive_int(poll_ms,'poll_ms')
        end=time.monotonic()+timeout_ms/1000
        while True:
            remaining=end-time.monotonic()
            if remaining<=0:raise WaitTimeout(operation_id)
            try:
                item=await asyncio.wait_for(self.get(operation_id),timeout=remaining)
            except TimeoutError as exc:raise WaitTimeout(operation_id) from exc
            result=_poll_result(item,operation_id)
            if result is not None:return result
            await asyncio.sleep(min(poll_ms/1000,max(0,end-time.monotonic())))

class _Environments:
    def __init__(self,c):self._c=c
    def prepare(self,plan:dict,*,idempotency_key:str):return self._c._send('POST','/v1/templates:prepare',body=plan,key=idempotency_key)
    def create(self,template_id:str,*,idempotency_key:str):return self._c._send('POST','/v1/environments',body={'template_id':identifier(template_id)},key=idempotency_key)
    def get(self,environment_id:str,*,generation:int):
        ref=SandboxRef(environment_id,generation)
        return self._c._send('GET',f'/v1/environments/{ref.sandbox_id}',ref=ref)
    def delete(self,environment_id:str,*,generation:int,idempotency_key:str):
        ref=SandboxRef(environment_id,generation)
        return self._c._send('DELETE',f'/v1/environments/{ref.sandbox_id}',ref=ref,key=idempotency_key)

class Client:
    def __init__(self,endpoint:str='unix:///run/lightsandbox/api.sock',*,token:str|None=None,transport=None):
        self._http=httpx.Client(**_connection(endpoint,token,transport,False))
        self.sandboxes=_Sandboxes(self);self.executions=_Executions(self);self.operations=_Operations(self);self.environments=_Environments(self)
    def _send(self,method,path,*,body=None,key=None,ref=None,request_timeout=None):
        h=_headers(key,ref)
        try:
            kw={'headers':h}
            if body is not None:kw['json']=body
            if request_timeout is not None:kw['timeout']=request_timeout
            r=self._http.request(method,path,**kw)
        except httpx.TransportError as exc:
            if method not in ('GET','HEAD'):raise AmbiguousRequest(key,method,path) from exc
            if request_timeout is not None and isinstance(exc,httpx.TimeoutException):raise WaitTimeout(path.rsplit('/',1)[-1]) from exc
            raise ApiError('TRANSPORT_ERROR','cannot reach sandbox API') from exc
        return _decode(r)
    def close(self):self._http.close()
    def __enter__(self):return self
    def __exit__(self,*args):self.close()

class AsyncClient:
    def __init__(self,endpoint:str='unix:///run/lightsandbox/api.sock',*,token:str|None=None,transport=None):
        self._http=httpx.AsyncClient(**_connection(endpoint,token,transport,True))
        self.sandboxes=_Sandboxes(self);self.executions=_Executions(self);self.operations=_AsyncOperations(self);self.environments=_Environments(self)
    async def _send(self,method,path,*,body=None,key=None,ref=None,request_timeout=None):
        h=_headers(key,ref)
        try:
            kw={'headers':h}
            if body is not None:kw['json']=body
            if request_timeout is not None:kw['timeout']=request_timeout
            r=await self._http.request(method,path,**kw)
        except httpx.TransportError as exc:
            if method not in ('GET','HEAD'):raise AmbiguousRequest(key,method,path) from exc
            raise ApiError('TRANSPORT_ERROR','cannot reach sandbox API') from exc
        return _decode(r)
    async def aclose(self):await self._http.aclose()
    async def __aenter__(self):return self
    async def __aexit__(self,*args):await self.aclose()
