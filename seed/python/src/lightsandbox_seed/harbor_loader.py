"""Harbor environment inventory without importing Harbor or running a task.

This scanner is deliberately NOT a second TaskConfig parser. It preserves unknown
fields as blockers; actual execution uses Harbor's locked Task/EnvironmentConfig.
No tests, solution, verifier secrets or instructions are uploaded to the agent.
"""
from __future__ import annotations
from dataclasses import dataclass,asdict
import json,tomllib
from pathlib import Path
from .bundle import Entry,snapshot_tree,read_regular_file

ENV_FIELDS=frozenset(('build_timeout_sec','docker_image','os','cpus','memory_mb','storage_mb','gpus','gpu_types','tpu','mcp_servers','env','skills_dir','healthcheck','workdir','allow_internet','memory','storage','network_mode','allowed_hosts'))

@dataclass(frozen=True)
class EnvironmentInspection:
    source_role:str
    source_path:str
    kind:str
    image:str|None
    image_selection:str
    upload_payload:bool
    content_digest:str
    entries:tuple[Entry,...]
    resources:dict
    env_keys:tuple[str,...]
    unknown_fields:tuple[str,...]

@dataclass(frozen=True)
class TaskInspection:
    name:str
    schema_version:str
    agent:EnvironmentInspection
    verifier_mode:str
    verifier:EnvironmentInspection|None
    required_features:tuple[str,...]
    authoritative_validation:bool=False
    execution_ready:bool=False
    def to_dict(self)->dict:return asdict(self)
    def to_json(self)->str:return json.dumps(self.to_dict(),ensure_ascii=False,sort_keys=True,indent=2)

def _map(x,field):
    if not isinstance(x,dict):raise ValueError(f'{field} must be a table')
    return x

def inspect_environment(directory:Path,config:dict,*,force_build:bool=False,role:str='agent')->EnvironmentInspection:
    directory=Path(directory);config=_map(config,'environment')
    if directory.is_symlink():raise ValueError('symlink environment directory')
    if not directory.is_dir():raise ValueError('missing environment directory')
    resources={k:config[k] for k in ('cpus','memory_mb','storage_mb','gpus') if k in config}
    for k,v in resources.items():
        minimum=0 if k=='gpus' else 1
        if type(v) is not int or v<minimum:raise ValueError(f'invalid resource: {k}')
    image=config.get('docker_image')
    if image is not None and (not isinstance(image,str) or not image.strip() or '\x00' in image):raise ValueError('invalid image')
    snapshot=snapshot_tree(directory);names={e.path for e in snapshot.entries}
    dockerfile='Dockerfile' in names;compose='docker-compose.yaml' in names
    if not image and not dockerfile and not compose:raise ValueError(f'no supported environment definition in {directory}')
    use_image=bool(image) and (not force_build or not dockerfile)
    kind='compose' if compose else ('image' if use_image else 'dockerfile')
    env=_map(config.get('env',{}),'environment.env')
    if any(not isinstance(k,str) or not isinstance(v,str) for k,v in env.items()):raise ValueError('env must map strings to strings')
    return EnvironmentInspection(role,str(directory),kind,image,'prebuilt' if use_image else 'build',
        bool(image and not dockerfile and not compose and snapshot.entries),snapshot.digest,
        snapshot.entries,resources,tuple(sorted(env)),tuple(sorted(set(config)-ENV_FIELDS)))

def inspect_task(task_dir:Path,*,force_build:bool=False)->TaskInspection:
    root=Path(task_dir)
    if root.is_symlink():raise ValueError('symlink task root')
    config_path=root/'task.toml'
    if config_path.is_symlink():raise ValueError('symlink task config')
    raw=tomllib.loads(read_regular_file(config_path).decode('utf-8'))
    env=_map(raw.get('environment',{}),'environment')
    agent=inspect_environment(root/'environment',env,force_build=force_build)
    info=_map(raw.get('task',{}),'task');name=info.get('name',root.name)
    if not isinstance(name,str) or not name:raise ValueError('invalid task name')
    version=str(raw.get('schema_version',raw.get('version','legacy')))
    verifier_config=_map(raw.get('verifier',{}),'verifier');has_v='environment' in verifier_config
    mode=verifier_config.get('environment_mode','separate' if has_v else 'shared')
    if mode not in ('shared','separate') or (mode=='shared' and has_v):raise ValueError('inconsistent verifier mode')
    features={'upstream_schema_validation'}
    if agent.kind=='compose':features.add('compose_normalization')
    if agent.image_selection=='build':features.add('buildkit')
    if agent.upload_payload:features.add('post_start_environment_upload')
    if agent.unknown_fields:features.add('unrecognized_environment_fields')
    if str(env.get('os','linux')).lower()!='linux':features.add('windows')
    if env.get('gpus',0):features.add('gpu')
    if env.get('tpu') is not None:features.add('tpu')
    if env.get('network_mode')=='allowlist':features.add('enforced_network_allowlist')
    if env.get('allow_internet') is not None or 'memory' in env or 'storage' in env:features.add('upstream_legacy_migration')
    if raw.get('steps'):features.add('harbor_multistep_orchestration')
    if verifier_config.get('collect'):features.add('service_collection')
    verifier=None
    if mode=='separate':
        features.add('separate_verifier')
        if has_v:verifier=inspect_environment(root/'tests',_map(verifier_config['environment'],'verifier.environment'),role='verifier')
        else:features.add('verifier_source_resolution')
    return TaskInspection(name,version,agent,mode,verifier,tuple(sorted(features)))
