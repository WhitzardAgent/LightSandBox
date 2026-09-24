"""Native v1 reference contracts. This module neither launches nor isolates processes."""
from __future__ import annotations
from dataclasses import dataclass
from copy import deepcopy
import re
from typing import Any, Mapping, Sequence

class ContractError(ValueError):
    pass

_ID = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')
def identifier(value: str) -> str:
    if not isinstance(value,str) or not _ID.fullmatch(value):
        raise ContractError('invalid opaque resource identifier')
    return value

def positive_int(value: int, name: str) -> int:
    if type(value) is not int or value <= 0:
        raise ContractError(f'{name} must be a positive integer')
    return value

def text(value: str, name: str, *, empty: bool=False) -> str:
    if not isinstance(value,str) or '\0' in value or (not value and not empty):
        raise ContractError(f'invalid {name}')
    return value

def vector(value: Sequence[str], *, allow_empty: bool=False) -> list[str]:
    if not isinstance(value,(list,tuple)) or (not value and not allow_empty):
        raise ContractError('argv must be an array')
    result=[text(x,'argument',empty=True) for x in value]
    if result and not result[0]:raise ContractError('executable cannot be empty')
    return result

def env_map(value: Mapping[str,str]) -> dict[str,str]:
    if not isinstance(value,Mapping):raise ContractError('env must be an object')
    result={}
    for k,v in value.items():
        text(k,'environment name');text(v,'environment value',empty=True)
        if '=' in k:raise ContractError('environment name contains =')
        result[k]=v
    return result

@dataclass(frozen=True)
class SandboxRef:
    sandbox_id:str
    generation:int
    def __post_init__(self):
        identifier(self.sandbox_id);positive_int(self.generation,'generation')
    def to_dict(self) -> dict:
        return {'sandbox_id':self.sandbox_id,'generation':self.generation}

class ExecSpec:
    """Exactly one command form. No implicit host or guest shell."""
    def __init__(self, *, argv:Sequence[str]|None=None, shell:str|None=None,
                 shell_executable:str|None=None, cwd:str|None=None,
                 env:Mapping[str,str]|None=None, timeout_ms:int|None=None,
                 user:str|None=None):
        if (argv is None)==(shell is None):raise ContractError('choose argv OR explicit shell')
        if argv is not None:
            if shell_executable is not None:raise ContractError('shell executable without shell')
            command={'kind':'argv','argv':vector(argv)}
        else:
            text(shell,'shell script',empty=True);text(shell_executable,'shell executable')
            if not shell_executable.startswith('/'):raise ContractError('shell executable must be guest-absolute')
            command={'kind':'shell','executable':shell_executable,'script':shell}
        if cwd is not None:
            text(cwd,'cwd')
            if not cwd.startswith('/'):raise ContractError('cwd must be guest-absolute')
        if timeout_ms is not None:positive_int(timeout_ms,'timeout_ms')
        if user is not None:text(user,'user')
        self._body={'command':command,'env':env_map({} if env is None else env)}
        if cwd is not None:self._body['cwd']=cwd
        if timeout_ms is not None:self._body['timeout_ms']=timeout_ms
        if user is not None:self._body['user']=user
    def to_dict(self) -> dict:
        return deepcopy(self._body)

@dataclass(frozen=True)
class ExitStatus:
    kind:str
    exit_code:int|None=None
    signal:int|None=None
    def __post_init__(self):
        if self.kind=='exited':
            if type(self.exit_code) is not int or not 0<=self.exit_code<=255 or self.signal is not None:
                raise ContractError('exited requires only a real exit code')
        elif self.kind=='signaled':
            if type(self.signal) is not int or not 1<=self.signal<=127 or self.exit_code is not None:
                raise ContractError('signaled requires only a signal')
        elif self.kind=='unknown':
            if self.exit_code is not None or self.signal is not None:raise ContractError('unknown has no result')
        else:raise ContractError('unknown exit status kind')
    def normalized_code(self) -> int:
        if self.kind=='unknown':raise ContractError('exit result unavailable; never assume zero')
        return self.exit_code if self.kind=='exited' else 128+self.signal

# Domain-specific states: execution exiting nonzero is still an observed `exited`.
STATE_MACHINES={
 'operation':{'pending':['running','failed'],'running':['succeeded','failed','unknown'],
              'unknown':[], 'succeeded':[], 'failed':[]},
 'execution':{'accepted':['starting','start-failed'],'starting':['running','start-failed','unknown'],
              'running':['exited','unknown'],'exited':[],'start-failed':[],'unknown':[]},
 'sandbox':{'requested':['preparing','failed'],'preparing':['created','failed'],
            'created':['starting','deleting'],'starting':['running','stopped','failed'],
            'running':['pausing','stopping','stopped'],'pausing':['paused','failed'],
            'paused':['resuming','stopping'],'resuming':['running','failed'],
            'stopping':['stopped','cleanup-required'],'stopped':['deleting'],
            'failed':['deleting'],'deleting':['deleted','cleanup-required'],
            'cleanup-required':['deleting'],'deleted':[]}}
def allowed_transition(domain:str,source:str,target:str,*,evidence:bool=False)->bool:
    if source=='unknown' and evidence and domain=='operation':return target in ('succeeded','failed')
    if source=='unknown' and evidence and domain=='execution':return target in ('exited','start-failed')
    return target in STATE_MACHINES.get(domain,{}).get(source,[])

def resolve_launch(image:dict, *, entrypoint:list[str]|None=None,
                   command:list[str]|None=None, mode:str='image')->dict:
    """Native image-config resolution, NOT Docker CLI flag interpretation.

    None inherits; [] clears. Docker/Compose/Harbor adapters implement their own
    flag/default rules *before* calling this function. Guest user lookup, mount
    creation, capabilities, image healthchecks and OCI config emission are not
    implemented by this reference function.
    """
    if not isinstance(image,dict):raise ContractError('image config must be object')
    ep=vector([] if image.get('Entrypoint') is None else image['Entrypoint'],allow_empty=True) if entrypoint is None else vector(entrypoint,allow_empty=True)
    cmd=vector([] if image.get('Cmd') is None else image['Cmd'],allow_empty=True) if command is None else vector(command,allow_empty=True)
    argv=ep+cmd
    cwd=image.get('WorkingDir')
    if cwd is None or cwd == '':cwd='/'
    text(cwd,'working directory')
    if not cwd.startswith('/'):raise ContractError('working directory must be absolute')
    user=image.get('User')
    if user is None or user == '':user='0'
    text(user,'image user')
    image_env=[] if image.get('Env') is None else image['Env']
    if not isinstance(image_env,list):raise ContractError('image Env must be an array')
    for item in image_env:text(item,'image environment value',empty=True)
    result={'argv':argv,'cwd':cwd,'user':user,'env':deepcopy(image_env),'mode':mode}
    if mode=='idle-agent':
        result['ignored_image_argv']=argv
        result['argv']=['/.lightsandbox/bin/init','--idle']
    elif mode!='image':raise ContractError('unsupported launch mode')
    elif not argv:raise ContractError('no process args')
    return result

def validate_environment(value:dict)->dict:
    """Strict *planning subset*; advanced fields are rejected, not discarded."""
    if not isinstance(value,dict):raise ContractError('environment must be object')
    allowed={'schema_version','services','networks','volumes','labels'}
    extra=set(value)-allowed
    if extra:raise ContractError(f'unknown environment fields: {sorted(extra)}')
    if value.get('schema_version','v1')!='v1':raise ContractError('unsupported schema version')
    services=value.get('services')
    if not isinstance(services,dict) or not services:raise ContractError('nonempty services required')
    for n,s in services.items():
        identifier(n)
        if not isinstance(s,dict):raise ContractError('service must be object')
        fields={'image','launch_mode','entrypoint','command','env','cwd','user','resources','mounts','networks','depends_on'}
        extra=set(s)-fields
        if extra:raise ContractError(f'unknown service fields: {sorted(extra)}')
        text(s.get('image'),'image')
        if s.get('launch_mode','image') not in ('image','idle-agent'):raise ContractError('unsupported launch mode')
        for field in ('entrypoint','command'):
            if field in s and s[field] is not None:vector(s[field],allow_empty=True)
        env_map(s.get('env',{}))
        if 'cwd' in s:
            text(s['cwd'],'cwd')
            if not s['cwd'].startswith('/'):raise ContractError('cwd must be absolute')
        if 'user' in s:text(s['user'],'user')
        r=s.get('resources',{})
        if not isinstance(r,dict) or set(r)-{'cpu_millis','memory_bytes','pids','storage_bytes'}:raise ContractError('unknown resources')
        for k,v in r.items():positive_int(v,k)
        # Volume/network/health schemas are reserved for the environment engine.
        # Nonempty data cannot be accepted until this subset has implementations.
        for f in ('mounts','networks','depends_on'):
            if f in s and s[f] not in ([],{}):raise ContractError(f'{f} not supported by offline subset')
    for f in ('networks','volumes'):
        if f in value and value[f]!={}:raise ContractError(f'{f} not supported by offline subset')
    if 'labels' in value:env_map(value['labels'])
    return deepcopy(value)
