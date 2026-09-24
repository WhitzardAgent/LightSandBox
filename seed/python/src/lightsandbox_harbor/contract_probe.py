"""Read-only AST drift detector, not an upstream lock or behavioral certification."""
import ast
import hashlib

EXPECTED = {
    '__init__': ('environment_dir','environment_name','session_id','trial_paths','task_env_config',
                 'persistent_env','mounts','network_policy','phase_network_policies','extra_docker_compose'),
    'start': ('force_build',), 'stop': ('delete',),
    'exec': ('command','cwd','env','timeout_sec','user'),
    'upload_file': ('source_path','target_path'), 'upload_dir': ('source_dir','target_dir'),
    'download_file': ('source_path','target_path'), 'download_dir': ('source_dir','target_dir'),
    '_merge_env': ('env',), '_resolve_user': ('user',), '_output_callback': (),
    '_upload_environment_dir_after_start': (), 'set_network_policy': ('network_policy',),
    'service_exec': ('command','service'), 'stop_service': ('service',),
}


def analyze_base_source(source: str) -> dict:
    if len(source.encode()) > 2 * 1024 * 1024:
        raise ValueError('source exceeds probe budget')
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        raise ValueError('invalid Python source') from exc
    cls = next((x for x in tree.body if isinstance(x, ast.ClassDef) and x.name == 'BaseEnvironment'), None)
    methods = {} if cls is None else {x.name:x for x in cls.body if isinstance(x,(ast.FunctionDef,ast.AsyncFunctionDef))}
    missing = sorted(set(EXPECTED)-methods.keys())
    drift = {}
    for name, args in EXPECTED.items():
        if name not in methods:
            continue
        node = methods[name]
        names = [x.arg for x in (*node.args.posonlyargs,*node.args.args,*node.args.kwonlyargs)]
        absent = sorted(set(args)-set(names))
        if absent:
            drift[name] = absent
    return {'source_sha256':hashlib.sha256(source.encode()).hexdigest(),
            'compatible_shape':cls is not None and not missing and not drift,
            'missing_methods':missing,'missing_parameters':drift,
            'behavior_verified':False,'immutable_source_locked':False}
