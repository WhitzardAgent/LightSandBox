"""Cross-layer schema checks. These validate contracts, not a server."""
import unittest,json
from pathlib import Path
from jsonschema import Draft202012Validator
from lightsandbox.contracts import ExecSpec, validate_environment, STATE_MACHINES
ROOT=Path(__file__).resolve().parents[2]
def load(path):return json.loads((ROOT/path).read_text())

class TestSchemas(unittest.TestCase):
    def test_all_standalone_schemas_well_formed(self):
        for p in (ROOT/'contracts/v1').glob('*.schema.json'):
            with self.subTest(path=p.name):Draft202012Validator.check_schema(json.loads(p.read_text()))
    def test_environment_example(self):
        x=load('examples/native/environment.json');validate_environment(x)
        Draft202012Validator(load('contracts/v1/environment.schema.json')).validate(x)
    def test_exec_generated_forms(self):
        v=Draft202012Validator(load('contracts/v1/exec.schema.json'))
        for s in [ExecSpec(argv=['printf','']),ExecSpec(shell='',shell_executable='/bin/sh'),ExecSpec(argv=['true'],env={'A':''},timeout_ms=1)]:v.validate(s.to_dict())
    def test_invalid_exec_variants(self):
        v=Draft202012Validator(load('contracts/v1/exec.schema.json'))
        for bad in [{'command':'echo x'},{'command':{'kind':'argv','argv':[]}},{'command':{'kind':'shell','script':'x'}},{'command':{'kind':'argv','argv':['x']},'timeout_ms':True}]:
            with self.subTest(bad=bad):self.assertTrue(list(v.iter_errors(bad)))
    def test_verifier_rejected_by_schema(self):
        v=Draft202012Validator(load('contracts/v1/environment.schema.json'))
        x=load('examples/native/environment.json');x['verifier']={}
        self.assertTrue(list(v.iter_errors(x)))
    def test_state_table_matches_code(self):
        self.assertEqual(load('contracts/v1/states.json')['domains'],STATE_MACHINES)
    def test_openapi_surface_alignment(self):
        api=load('contracts/v1/native.openapi.json');surface=load('contracts/v1/surface.json')
        actual={(m.upper(),p):o for p,ms in api['paths'].items() for m,o in ms.items()}
        self.assertEqual(set(actual),{(x['method'],x['path']) for x in surface['http']})
        for x in surface['http']:
            o=actual[x['method'],x['path']]
            self.assertEqual(x['operation_id'],o['operationId'])
            self.assertIn(str(x['response_status']),o['responses'])
            self.assertEqual(o['x-server-status'],'NOT_IMPLEMENTED')
            headers={h['name'] for h in o['parameters'] if h['in']=='header'}
            self.assertEqual('X-LS-Generation' in headers,x['requires_generation'])
            self.assertEqual('Idempotency-Key' in headers,x['method']!='GET')
    def test_openapi_error_envelope_matches_frozen_schema(self):
        api=load('contracts/v1/native.openapi.json');frozen=load('contracts/v1/error.schema.json')
        o=api['components']['schemas']['Error']
        Draft202012Validator.check_schema(o)
        vo=Draft202012Validator(o);vf=Draft202012Validator(frozen)
        inner=frozen['properties']['error']
        probes=[{'error':{'code':'NOT_FOUND','message':'gone','request_id':'rq1'}},
                {'error':{'code':'NOT_A_CODE','message':'x','request_id':'rq1'}},
                {'error':{'code':'NOT_FOUND','message':'x'}},
                {'error':{'code':'NOT_FOUND','message':'','request_id':'rq1'}},
                {'error':{'code':'NOT_FOUND','message':'x','request_id':''}},
                {'error':{'code':'NOT_FOUND','message':'x','request_id':'bad\u0000id'}},
                {'error':{'code':'NOT_FOUND','message':'x','request_id':'rq1','extra':1}},
                {'error':{'code':'NOT_FOUND','message':'x','request_id':'rq1'},'extra':1},
                {}]
        for p in probes:
            with self.subTest(probe=p):
                self.assertEqual(not list(vo.iter_errors(p)),not list(vf.iter_errors(p)))
        err=o['properties']['error']
        self.assertEqual(set(err['required']),set(inner['required']))
        self.assertEqual(err['properties']['code']['enum'],inner['properties']['code']['enum'])
    def test_local_refs_resolve(self):
        api=load('contracts/v1/native.openapi.json')
        def visit(x):
            if isinstance(x,dict):
                if '$ref' in x:
                    ref=x['$ref'];self.assertTrue(ref.startswith('#/'));v=api
                    for part in ref[2:].split('/'):v=v[part]
                for v in x.values():visit(v)
            elif isinstance(x,list):
                for v in x:visit(v)
        visit(api)
    def test_status_union_no_fake_zero(self):
        schema=load('contracts/v1/native.openapi.json')['components']['schemas']['ExitStatus']
        v=Draft202012Validator(schema)
        self.assertTrue(list(v.iter_errors({'kind':'unknown','exit_code':0})))
        v.validate({'kind':'exited','exit_code':23})

class TestSDKRouteCoverage(unittest.TestCase):
    def test_all_implemented_routes_emit_contract_requests(self):
        import httpx,re
        from lightsandbox import Client,SandboxRef,ExecSpec
        observed=[]
        def handler(req):
            observed.append(req)
            return httpx.Response(202 if req.method!='GET' else 200,json={'id':'demo','status':'pending'})
        with Client('https://node.invalid',transport=httpx.MockTransport(handler)) as c:
            ref=SandboxRef('sb_one',7)
            c.sandboxes.create({'image':'example.invalid/demo'},idempotency_key='k1')
            c.sandboxes.get(ref);c.sandboxes.delete(ref,idempotency_key='k2')
            c.executions.start(ref,ExecSpec(argv=['true']),idempotency_key='k3')
            c.executions.get('ex_one');c.executions.cancel('ex_one',scope='tree',idempotency_key='k4')
            c.operations.get('op_one')
            plan={'plan_version':'v1','spec':load('examples/native/environment.json'),'plan_digest':'sha256:'+'a'*64,'execution_ready':False,'executed':False,'unresolved':['test only']}
            c.environments.prepare(plan,idempotency_key='k5');c.environments.create('tpl_one',idempotency_key='k6')
            c.environments.get('env_one',generation=3);c.environments.delete('env_one',generation=3,idempotency_key='k7')
        surface=load('contracts/v1/surface.json')['http'];api=load('contracts/v1/native.openapi.json')
        self.assertEqual(len(observed),len(surface))
        seen=set()
        for r in observed:
            candidates=[x for x in surface if x['method']==r.method and re.fullmatch(re.escape(x['path']).replace(r'\{id\}',r'[^/]+'),r.url.path)]
            self.assertEqual(len(candidates),1)
            op=candidates[0];seen.add(op['operation_id'])
            self.assertEqual('X-LS-Generation' in r.headers,op['requires_generation'])
            self.assertEqual('Idempotency-Key' in r.headers,r.method!='GET')
            if op['request_schema']:
                schema=api['components']['schemas'][op['request_schema']]
                Draft202012Validator(schema).validate(json.loads(r.content))
        self.assertEqual(seen,{x['operation_id'] for x in surface})
