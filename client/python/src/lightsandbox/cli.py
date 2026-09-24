"""Native CLI reference: offline planning and real HTTP request submission.

A running compatible daemon is required for non-dry-run commands. No host shell,
Docker fallback, fake command execution, or implicit cleanup is used.
"""
from __future__ import annotations
import argparse,hashlib,json,os,sys,uuid
from pathlib import Path
from .contracts import ExecSpec,SandboxRef,validate_environment
from .client import Client,ApiError,AmbiguousRequest,WaitTimeout,OperationUnknown

class UsageError(ValueError):pass
class Parser(argparse.ArgumentParser):
    def error(self,message):raise UsageError(message)

def _parser():
    p=Parser(prog='lsbx',description=__doc__)
    p.add_argument('--endpoint',default=os.getenv('LIGHTSANDBOX_ENDPOINT','unix:///run/lightsandbox/api.sock'))
    p.add_argument('--json',action='store_true',dest='json_mode')
    sub=p.add_subparsers(dest='command',required=True)
    sub.add_parser('version')
    sp=sub.add_parser('sandbox').add_subparsers(dest='action',required=True)
    c=sp.add_parser('create');c.add_argument('--image',required=True);c.add_argument('--launch-mode',choices=['image','idle-agent'],default='image');c.add_argument('--idempotency-key');c.add_argument('--dry-run',action='store_true')
    for cmd in ('get','delete'):
        c=sp.add_parser(cmd);c.add_argument('sandbox_id');c.add_argument('--generation',required=True,type=int)
        if cmd=='delete':c.add_argument('--idempotency-key')
        c.add_argument('--dry-run',action='store_true')
    e=sub.add_parser('exec');e.add_argument('sandbox_id');e.add_argument('--generation',required=True,type=int)
    e.add_argument('--shell');e.add_argument('--shell-executable');e.add_argument('--cwd');e.add_argument('--timeout-ms',type=int)
    e.add_argument('--env',action='append',default=[]);e.add_argument('--idempotency-key');e.add_argument('--dry-run',action='store_true')
    op=sub.add_parser('op').add_subparsers(dest='action',required=True)
    for cmd in ('get','wait'):
        a=op.add_parser(cmd);a.add_argument('operation_id')
        if cmd=='wait':a.add_argument('--wait-timeout-ms',type=int,default=30000)
    env=sub.add_parser('env').add_subparsers(dest='action',required=True)
    ep=env.add_parser('plan');ep.add_argument('--file',type=Path,required=True)
    return p

def main(argv=None,*,stdout=None,stderr=None)->int:
    stdout=stdout or sys.stdout;stderr=stderr or sys.stderr
    args=list(sys.argv[1:] if argv is None else argv)
    json_mode='--json' in args
    try:
        tail=None
        if '--' in args:
            i=args.index('--');tail=args[i+1:];args=args[:i]
        ns=_parser().parse_args(args)
        if tail is not None and ns.command!='exec':raise UsageError('-- argv is accepted only by exec')
        if ns.command=='version':
            answer={'client_version':'0.5.0.dev0','api_version':'v1','runtime_implemented':False,'scope':'client-and-contract-reference'}
        elif ns.command=='env':
            with ns.file.open('rb') as f:raw=f.read(2*1024*1024+1)
            if len(raw)>2*1024*1024:raise UsageError('plan input exceeds 2 MiB')
            spec=validate_environment(json.loads(raw))
            encoded=json.dumps(spec,sort_keys=True,separators=(',',':'),ensure_ascii=False,allow_nan=False).encode()
            unresolved=['host capabilities not probed','images not materialized','template not prepared']
            for name,s in spec['services'].items():
                if 'sha256:' not in s['image']:unresolved.append(f'image reference requires digest resolution: {name}')
            answer={'plan_version':'v1','spec':spec,'plan_digest':'sha256:'+hashlib.sha256(encoded).hexdigest(),
                    'execution_ready':False,'unresolved':unresolved,'executed':False}
        else:
            key=getattr(ns,'idempotency_key',None) or ('dry-run-key' if getattr(ns,'dry_run',False) else uuid.uuid4().hex)
            method='GET';path='';body=None;ref=None
            if ns.command=='exec':
                env={}
                for entry in ns.env:
                    if '=' not in entry:raise UsageError('--env requires NAME=VALUE')
                    k,v=entry.split('=',1)
                    if k in env:raise UsageError('duplicate --env name')
                    env[k]=v
                ref=SandboxRef(ns.sandbox_id,ns.generation)
                body=ExecSpec(argv=tail,shell=ns.shell,shell_executable=ns.shell_executable,cwd=ns.cwd,env=env,timeout_ms=ns.timeout_ms).to_dict()
                method='POST';path=f'/v1/sandboxes/{ref.sandbox_id}/executions'
            elif ns.command=='sandbox':
                if ns.action=='create':method='POST';path='/v1/sandboxes';body={'image':ns.image,'launch_mode':ns.launch_mode}
                else:
                    ref=SandboxRef(ns.sandbox_id,ns.generation);path=f'/v1/sandboxes/{ref.sandbox_id}'
                    if ns.action=='delete':method='DELETE'
            if getattr(ns,'dry_run',False):
                if isinstance(body,dict) and 'env' in body:
                    body={**body,'env':{k:'<redacted>' for k in body['env']}}
                answer={'method':method,'path':path,'body':body,'generation':ref.generation if ref else None,
                        'idempotency_key':key if method!='GET' else None,'executed':False}
            else:
                with Client(ns.endpoint,token=os.getenv('LIGHTSANDBOX_TOKEN')) as c:
                    if ns.command=='op':
                        answer=c.operations.wait(ns.operation_id,timeout_ms=ns.wait_timeout_ms) if ns.action=='wait' else c.operations.get(ns.operation_id)
                    else:answer=c._send(method,path,body=body,key=key if method!='GET' else None,ref=ref)
        print(json.dumps(answer,ensure_ascii=False,indent=None if ns.json_mode else 2,allow_nan=False),file=stdout)
        return 0
    except (ValueError,OSError) as exc:
        error={'error':{'code':'INVALID_ARGUMENT','message':str(exc)}};code=2
    except ApiError as exc:
        code=10 if isinstance(exc,(AmbiguousRequest,OperationUnknown)) else 8 if isinstance(exc,WaitTimeout) else 11
        error={'error':{'code':exc.code,'message':str(exc),'request_id':exc.request_id}}
        if isinstance(exc,AmbiguousRequest):error['error']['idempotency_key']=exc.idempotency_key
    print(json.dumps(error,ensure_ascii=False) if json_mode else error['error']['message'],file=stderr)
    return code

if __name__=='__main__':raise SystemExit(main())
