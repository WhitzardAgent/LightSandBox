"""Per-task capability census.

Parses the real task definition files (task.toml, docker-compose.yaml,
Dockerfiles) of a benchmark checkout into one record per task, preserving —
never silently dropping — fields the inventory does not model. Aggregates
records into the field histogram written to specs/task-feature-census.json.
"""
from __future__ import annotations

import json
from pathlib import Path
import tomllib

from . import yamlmini

KNOWN_COMPOSE_TOP = {
    'name', 'services', 'volumes', 'networks', 'configs', 'secrets', 'version',
    'include', 'extends',
}
KNOWN_SERVICE_KEYS = {
    'image', 'build', 'devices', 'ulimits', 'shm_size', 'cap_add', 'cap_drop',
    'environment', 'env_file', 'volumes', 'networks', 'healthcheck', 'ports',
    'expose', 'depends_on', 'privileged', 'entrypoint', 'command', 'restart',
    'working_dir', 'deploy', 'security_opt', 'sysctls', 'labels', 'hostname',
    'user', 'read_only', 'init', 'stop_signal', 'dns', 'extra_hosts', 'logging',
    'container_name', 'profiles', 'pull_policy', 'platform', 'mem_limit',
    'memswap_limit', 'cpus', 'cpu_shares', 'pids_limit', 'oom_score_adj',
    'ipc', 'pid', 'userns_mode', 'cgroup', 'cgroup_parent', 'runtime', 'scale',
    'tty', 'stdin_open', 'links', 'external_links', 'network_mode',
    'volumes_from', 'tmpfs', 'extends', 'develop', 'annotations', 'attach',
    'blkio_config', 'configs', 'secrets', 'credential_spec', 'device_cgroup_rules',
    'dns_opt', 'dns_search', 'domainname', 'gpus', 'group_add', 'isolation',
    'kernel_memory', 'label_file', 'mac_address', 'mounts', 'networks',
    'post_start', 'pre_stop', 'provider', 'pull_refresh_after', 'storage_opt',
    'stop_grace_period', 'uts', 'models', 'hooks', 'interface_name', 'extends',
}
KNOWN_TASK_TOML_TOP = {
    'task', 'metadata', 'verifier', 'agent', 'environment', 'artifacts',
    'schema_version', 'solution',
}
KNOWN_TASK_TOML_ENV = {
    'build_timeout_sec', 'cpus', 'memory_mb', 'storage_mb', 'gpus',
    'docker_image', 'allow_internet', 'mcp_servers', 'docker_context',
    'docker_compose_path', 'dockerfile_path', 'env', 'skills_dir',
    'gpu_types', 'healthcheck',
}
KNOWN_TASK_TOML_VERIFIER = {
    'timeout_sec', 'environment_mode', 'environment', 'env', 'collect',
}

ORACLE_PIECE_NAMES = ('solution', 'oracle', 'solve', 'gold')


def _repr(value):
    # Scalars are kept as values; containers keep a stable JSON text form.
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return json.dumps(value, sort_keys=True, ensure_ascii=False, default=str)


def _collect(task_dir: Path) -> dict[str, Path]:
    files = {}
    for p in sorted(task_dir.rglob('*')):
        if p.is_file() and '.git' not in p.parts:
            files[p.relative_to(task_dir).as_posix()] = p
    return files


def scan_task(task_dir, benchmark: str, split: str) -> dict:
    """One census record for a single benchmark task directory."""
    task_dir = Path(task_dir)
    record = {
        'benchmark': benchmark,
        'split': split,
        'task_id': task_dir.name,
        'uid': f'{benchmark}:{split}:{task_dir.name}',
        'fields': {},
        'unknown_keys': [],
        'parse_errors': [],
    }
    files = _collect(task_dir)
    fields = record['fields']

    toml_path = files.get('task.toml')
    task_toml = None
    if toml_path is not None:
        try:
            task_toml = tomllib.loads(toml_path.read_text(encoding='utf-8'))
        except (tomllib.TOMLDecodeError, UnicodeDecodeError) as exc:
            record['parse_errors'].append({'file': 'task.toml', 'error': str(exc)})
    if isinstance(task_toml, dict):
        # schema_version is the harbor task-format version at this rev;
        # absent means a pre-versioning task file. Recorded verbatim.
        if task_toml.get('schema_version') is not None:
            fields['task_schema_version'] = str(task_toml['schema_version'])
        fields['has_instruction'] = _has(files, 'instruction.md')
        for key in task_toml:
            if key not in KNOWN_TASK_TOML_TOP:
                record['unknown_keys'].append({
                    'file': 'task.toml', 'key': key, 'value': _repr(task_toml[key])})
        if isinstance(task_toml.get('solution'), dict):
            fields['solution_toml_section'] = True
        env = task_toml.get('environment') or {}
        if isinstance(env, dict):
            for key in env:
                if key not in KNOWN_TASK_TOML_ENV:
                    record['unknown_keys'].append({
                        'file': 'task.toml', 'key': f'environment.{key}',
                        'value': _repr(env[key])})
            fields['environment'] = {
                k: env.get(k) for k in ('cpus', 'memory_mb', 'storage_mb', 'gpus')
                if env.get(k) is not None
            }
            if env.get('allow_internet') is not None:
                fields['allow_internet'] = bool(env['allow_internet'])
            if env.get('mcp_servers'):
                fields['mcp_servers'] = env['mcp_servers']
            if env.get('env'):
                fields['env_vars'] = sorted(env['env'].keys()) \
                    if isinstance(env['env'], dict) else list(env['env'])
            if env.get('skills_dir'):
                fields['skills_dir'] = str(env['skills_dir'])
            if env.get('gpu_types'):
                fields['gpu_types'] = list(env['gpu_types']) \
                    if isinstance(env['gpu_types'], list) else [str(env['gpu_types'])]
            if env.get('healthcheck'):
                fields['environment_healthcheck'] = True
        verifier = task_toml.get('verifier') or {}
        if isinstance(verifier, dict):
            for key in verifier:
                if key not in KNOWN_TASK_TOML_VERIFIER:
                    record['unknown_keys'].append({
                        'file': 'task.toml', 'key': f'verifier.{key}',
                        'value': _repr(verifier[key])})
        fields['verifier_topology'] = verifier.get('environment_mode') or 'unspecified'
        if verifier.get('timeout_sec') is not None:
            fields['verifier_timeout_sec'] = verifier['timeout_sec']
        if verifier.get('env'):
            fields['verifier_env_vars'] = sorted(verifier['env'].keys()) \
                if isinstance(verifier['env'], dict) else list(verifier['env'])
        if verifier.get('collect') is not None:
            fields['verifier_collect'] = verifier['collect']
        artifacts = task_toml.get('artifacts') or []
        if artifacts:
            fields['artifacts'] = list(artifacts)
        metadata = task_toml.get('metadata') or {}
        if isinstance(metadata, dict) and metadata.get('tags'):
            fields['tags'] = list(metadata['tags'])
        if isinstance(metadata, dict) and metadata.get('category'):
            fields['category'] = metadata['category']
    else:
        fields['no_task_toml'] = True

    # Solution / oracle files: any file under a directory whose name looks
    # like an oracle piece (solution/, oracle/...).
    solution_files = sorted(
        rel for rel in files
        if any(part in ORACLE_PIECE_NAMES for part in Path(rel).parts[:-1])
        or Path(rel).name in ('solve.sh', 'solve.py', 'solution.sh', 'solution.yaml',
                              'solution.py', 'oracle.sh'))
    if fields.get('solution_toml_section'):
        solution_files = sorted(solution_files + ['task.toml#solution'])
    fields['solution_files'] = solution_files
    fields['has_verifier_tests'] = any(rel.startswith('tests/') for rel in files)

    # Dockerfiles: FROM bases, entrypoint, non-root USER.
    dockerfiles = {}
    for rel, path in files.items():
        name = Path(rel).name
        if name.startswith('Dockerfile'):
            try:
                text = path.read_text(encoding='utf-8')
            except UnicodeDecodeError:
                record['parse_errors'].append({'file': rel, 'error': 'not utf-8'})
                continue
            bases, users = [], []
            entrypoint = False
            for line in text.splitlines():
                stmt = line.strip()
                if stmt.startswith('#') or not stmt:
                    continue
                head = stmt.split(None, 1)[0].upper() if stmt.split(None, 1) else ''
                if head == 'FROM':
                    bases.append(stmt.split(None, 1)[1].split(' AS ')[0].split(' as ')[0])
                elif head == 'USER':
                    users.append(stmt.split(None, 1)[1])
                elif head in ('ENTRYPOINT', 'CMD'):
                    entrypoint = True
            dockerfiles[rel] = {
                'base_images': bases,
                'user_statements': users,
                'has_entrypoint_or_cmd': entrypoint,
            }
    fields['dockerfiles'] = dockerfiles

    # Compose files.
    composes = {}
    for rel, path in files.items():
        name = Path(rel).name.lower()
        if name in ('docker-compose.yaml', 'docker-compose.yml', 'compose.yaml', 'compose.yml'):
            try:
                text = path.read_text(encoding='utf-8')
            except UnicodeDecodeError:
                record['parse_errors'].append({'file': rel, 'error': 'not utf-8'})
                continue
            try:
                parsed = yamlmini.loads(text)
            except yamlmini.MiniYamlError as exc:
                record['parse_errors'].append({'file': rel, 'error': str(exc)})
                composes[rel] = {'unparsed': True}
                continue
            if not isinstance(parsed, dict):
                composes[rel] = {'unparsed': True}
                record['parse_errors'].append({'file': rel, 'error': 'top level not a mapping'})
                continue
            composes[rel] = _compose_facts(parsed, rel, record)
    fields['composes'] = composes
    return record


def _has(files, name):
    return any(Path(rel).name == name for rel in files)


def _compose_facts(parsed, rel, record):
    """Flatten one parsed compose document into capability facts; unknown
    keys at top level and service level are preserved verbatim."""
    facts = {
        'services': {},
        'top_volumes': sorted((parsed.get('volumes') or {}).keys())
        if isinstance(parsed.get('volumes'), dict) else [],
        'top_networks': sorted((parsed.get('networks') or {}).keys())
        if isinstance(parsed.get('networks'), dict) else [],
    }
    for key in parsed:
        if key not in KNOWN_COMPOSE_TOP and not key.startswith('x-'):
            record['unknown_keys'].append(
                {'file': rel, 'key': key, 'value': _repr(parsed[key])})
    for key in parsed:
        if key.startswith('x-'):
            record['unknown_keys'].append(
                {'file': rel, 'key': key, 'value': _repr(parsed[key]),
                 'extension': True})
    services = parsed.get('services') or {}
    if not isinstance(services, dict):
        record['parse_errors'].append({'file': rel, 'error': 'services not a mapping'})
        return facts
    for svc_name, svc in services.items():
        svc = svc or {}
        if not isinstance(svc, dict):
            record['parse_errors'].append(
                {'file': rel, 'error': f'service {svc_name} not a mapping'})
            continue
        entry = {}
        for key in svc:
            if key not in KNOWN_SERVICE_KEYS and not key.startswith('x-'):
                record['unknown_keys'].append({
                    'file': rel, 'key': f'services.{svc_name}.{key}',
                    'value': _repr(svc[key])})
        for key in ('devices', 'cap_add', 'cap_drop', 'expose', 'ports', 'sysctls',
                    'security_opt', 'entrypoint', 'environment', 'depends_on',
                    'dns', 'extra_hosts', 'labels', 'volumes_from', 'tmpfs'):
            if svc.get(key):
                entry[key] = svc[key] if not isinstance(svc[key], (dict, list)) else svc[key]
        if svc.get('image'):
            entry['image'] = svc['image']
        if svc.get('build') is not None:
            entry['build'] = svc['build']
        if svc.get('privileged') is not None:
            entry['privileged'] = bool(svc['privileged'])
        if svc.get('ulimits'):
            entry['ulimits'] = svc['ulimits']
        if svc.get('shm_size') is not None:
            entry['shm_size'] = svc['shm_size']
        if svc.get('healthcheck'):
            entry['healthcheck'] = svc['healthcheck']
        if svc.get('volumes'):
            entry['volumes'] = svc['volumes']
        if svc.get('networks'):
            entry['networks'] = svc['networks']
        if svc.get('network_mode'):
            entry['network_mode'] = svc['network_mode']
        for key in ('cpus', 'mem_limit', 'pids_limit', 'platform', 'user',
                    'working_dir', 'restart', 'tty', 'stdin_open', 'read_only'):
            if svc.get(key) is not None:
                entry[key] = svc[key]
        facts['services'][str(svc_name)] = entry
    return facts


def scan_checkout(checkout, benchmark: str, split: str) -> list[dict]:
    """One record per task directory under <checkout>/tasks (harbor layout)."""
    checkout = Path(checkout)
    tasks_dir = checkout / 'tasks'
    records = []
    if not tasks_dir.is_dir():
        return records
    for child in sorted(tasks_dir.iterdir()):
        if child.is_dir() and (child / 'task.toml').is_file():
            records.append(scan_task(child, benchmark, split))
    return records


def record_capabilities(record: dict) -> dict:
    """Flatten one census record into the manifest capability block.

    `demands` is the sorted set of non-trivial runtime requirements; every
    observed field also appears as data so nothing is lost between census and
    manifest."""
    fields = record.get('fields', {})
    caps = {
        'demands': set(),
        'verifier_topology': fields.get('verifier_topology', 'unspecified'),
        'compose_services': [],
        'healthchecks': False,
    }
    if fields.get('dockerfiles'):
        caps['base_images'] = sorted({
            base for d in fields['dockerfiles'].values() for base in d['base_images']})
        caps['dockerfile_users'] = sorted({
            u for d in fields['dockerfiles'].values() for u in d['user_statements']})
    compose_present = False
    devices, cap_add, cap_drop = [], [], []
    ulimits, environment, volumes, networks = {}, {}, [], []
    depends_on = False
    privileged = False
    shm_size = None
    network_modes = set()
    ports = []
    for compose in (fields.get('composes') or {}).values():
        if not compose or compose.get('unparsed'):
            continue
        compose_present = True
        for svc_name, svc in compose['services'].items():
            caps['compose_services'].append(svc_name)
            if svc.get('devices'):
                devices.extend(str(d) for d in svc['devices'])
            if svc.get('ulimits'):
                ulimits.update(svc['ulimits'])
            if svc.get('shm_size') is not None:
                shm_size = str(svc['shm_size'])
            for cap in svc.get('cap_add') or []:
                cap_add.append(str(cap))
            for cap in svc.get('cap_drop') or []:
                cap_drop.append(str(cap))
            if svc.get('environment'):
                environment.update(svc['environment'] if isinstance(svc['environment'], dict)
                                   else {str(v): None for v in svc['environment']})
            if svc.get('volumes'):
                volumes.extend(str(v) for v in svc['volumes'])
            if svc.get('networks'):
                nets = svc['networks']
                networks.extend(map(str, nets if isinstance(nets, list) else nets.keys()))
            if svc.get('healthcheck'):
                caps['healthchecks'] = True
            if svc.get('depends_on'):
                depends_on = True
            if svc.get('privileged'):
                privileged = True
            if svc.get('network_mode'):
                network_modes.add(str(svc['network_mode']))
            if svc.get('ports'):
                ports.extend(str(p) for p in svc['ports'])
    if compose_present:
        caps['demands'].add('compose')
    dockerfiles_env = {rel: f for rel, f in (fields.get('dockerfiles') or {}).items()
                       if rel.startswith('environment/')}
    compose_build = any(
        not c.get('unparsed') and any(s.get('build') is not None
                                      for s in c['services'].values())
        for c in (fields.get('composes') or {}).values() if c)
    if dockerfiles_env or compose_build:
        caps['demands'].add('dockerfile')
    if devices:
        caps['devices'] = sorted(set(devices))
        caps['demands'].add('devices')
    if ulimits:
        caps['ulimits'] = ulimits
        caps['demands'].add('ulimits')
    if shm_size is not None:
        caps['shm_size'] = shm_size
        caps['demands'].add('shm_size')
    if cap_add:
        caps['cap_add'] = sorted(set(cap_add))
        caps['demands'].add('cap_add')
    if cap_drop:
        caps['cap_drop'] = sorted(set(cap_drop))
        caps['demands'].add('cap_drop')
    if environment:
        caps['environment'] = dict(sorted(environment.items()))
        caps['demands'].add('environment')
    if volumes:
        caps['volumes'] = sorted(set(volumes))
        caps['demands'].add('volumes')
    if networks:
        caps['networks'] = sorted(set(networks))
        caps['demands'].add('networks')
    if caps['healthchecks']:
        caps['demands'].add('healthchecks')
    if depends_on:
        caps['demands'].add('depends_on')
    if privileged:
        caps['privileged'] = True
        caps['demands'].add('privileged')
    if network_modes:
        caps['network_modes'] = sorted(network_modes)
        caps['demands'].add('network_mode')
    if ports:
        caps['ports'] = sorted(set(ports))
        caps['demands'].add('ports')
    if len(caps['compose_services']) > 1:
        caps['demands'].add('multi_service')
    caps['compose_services'] = sorted(set(caps['compose_services']))
    env = fields.get('environment') or {}
    for key in ('cpus', 'memory_mb', 'storage_mb', 'gpus'):
        if env.get(key) is not None:
            caps[key] = env[key]
    if env.get('gpus'):
        caps['demands'].add('gpus')
        if fields.get('gpu_types'):
            caps['gpu_types'] = fields['gpu_types']
    if fields.get('mcp_servers'):
        caps['mcp_servers'] = fields['mcp_servers']
        caps['demands'].add('mcp_servers')
    if fields.get('env_vars'):
        caps['env_vars'] = fields['env_vars']
        caps['demands'].add('env_vars')
    if fields.get('skills_dir'):
        caps['skills_dir'] = fields['skills_dir']
        caps['demands'].add('skills_dir')
    if fields.get('environment_healthcheck'):
        caps['healthchecks'] = True
        caps['demands'].add('healthchecks')
    if fields.get('verifier_env_vars'):
        caps['verifier_env_vars'] = fields['verifier_env_vars']
    if fields.get('allow_internet') is not None:
        caps['allow_internet'] = fields['allow_internet']
    if fields.get('task_schema_version'):
        caps['task_schema_version'] = fields['task_schema_version']
    if fields.get('has_verifier_tests'):
        caps['demands'].add('verifier_tests')
    caps['demands'] = sorted(caps['demands'])
    return caps


ENTRY_VALUE_FIELDS = {
    'swebench': ('repo',),
    'cybergym': ('project_language',),
    'terminalbench': (),
}


def record_from_entry(entry: dict) -> dict:
    """Census record for benchmarks whose tasks are dataset rows rather than
    local file trees (SWE-bench instances, CyberGym tasks). The capability
    block of the manifest entry is the extraction result; `value_fields`
    feeds per-benchmark value histograms (repos, languages)."""
    caps = entry.get('capabilities', {})
    values = {}
    for field in ENTRY_VALUE_FIELDS.get(entry.get('benchmark'), ()):
        if caps.get(field) is not None:
            values[field] = caps[field]
    return {
        'benchmark': entry.get('benchmark'),
        'split': entry.get('split'),
        'task_id': entry.get('task_id'),
        'uid': entry.get('uid'),
        'fields': {},
        'unknown_keys': [],
        'capabilities': caps,
        'value_fields': values,
    }


def _value_histogram(counter):
    return {str(k): counter[k] for k in sorted(counter, key=str)}


def build_census(records: list[dict]) -> dict:
    """Aggregate per-task records into the census document."""
    from collections import Counter

    records = sorted(records, key=lambda r: r['uid'])
    by_benchmark = {}
    global_counters = {}

    def bump(store, field, value):
        store.setdefault(field, Counter())[str(value)] += 1

    presence_field = {'network_modes': 'network_mode'}

    for rec in records:
        bench = rec['benchmark']
        caps = rec.get('capabilities') or record_capabilities(rec)
        demands = set(caps.get('demands', []))
        store = by_benchmark.setdefault(bench, {})
        for counter in (store, global_counters):
            bump(counter, 'verifier_topology', caps.get('verifier_topology', 'unspecified'))
            for demand in sorted(demands):
                counter.setdefault('demand', Counter())[demand] += 1
            for field in ('devices', 'ulimits', 'shm_size', 'cap_add', 'cap_drop',
                          'volumes', 'networks', 'environment', 'ports',
                          'healthchecks', 'privileged', 'network_modes', 'compose',
                          'dockerfile', 'multi_service'):
                if presence_field.get(field, field) in demands:
                    counter.setdefault(field, Counter())['tasks'] += 1
            for num in ('cpus', 'memory_mb', 'storage_mb', 'gpus'):
                if caps.get(num) is not None:
                    bump(counter, num, caps[num])
        for key, value in sorted((rec.get('value_fields') or {}).items()):
            bump(store, f'value:{key}', value)
        per_bench = store.setdefault('_unknown', [])
        per_bench.extend({**u, 'benchmark': bench, 'task_id': rec['task_id']}
                         for u in rec['unknown_keys'])

    def render(store):
        out = {}
        for field in sorted(store):
            if field.startswith('_'):
                continue
            out[field] = _value_histogram(store[field])
        return out

    unknown_all = []
    benchmarks = {}
    for bench in sorted(by_benchmark):
        store = by_benchmark[bench]
        unknown = sorted(store.get('_unknown', []),
                         key=lambda u: (u['task_id'], u['file'], u['key']))
        unknown_all.extend(unknown)
        benchmarks[bench] = {
            'tasks': sum(1 for r in records if r['benchmark'] == bench),
            'field_histogram': render(store),
            'unknown_keys': unknown,
        }
    return {
        'schema_version': '1.0',
        'totals': {'tasks': len(records),
                   'by_benchmark': {b: benchmarks[b]['tasks'] for b in benchmarks}},
        'field_histogram': render(global_counters),
        'by_benchmark': benchmarks,
        'unknown_keys': unknown_all,
    }


def census_bytes(census: dict) -> bytes:
    return (json.dumps(census, sort_keys=True, ensure_ascii=False, indent=2)
            + '\n').encode('utf-8')
