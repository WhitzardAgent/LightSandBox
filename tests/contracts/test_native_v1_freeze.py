"""T41 native contract freeze — cross-language fixture tests (Python side).

Engines that must agree on every fixture:
  A. JSON schema (contracts/v1/*.schema.json) via jsonschema
  B. Python strict reference (tests/contracts/native_v1_ref.py)
  C. Rust serde types + resolver (crates/ls-core/src/contracts/) — exercised by
     `cargo test -p ls-core` over the SAME fixture files

Cases T41-C01..C05 from plans/tasks/T41.md plus the round-02 capability
resolution rejects (HOST_CAPABILITY_UNAVAILABLE / POLICY_UNSUPPORTED).
"""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

import jsonschema

sys.path.insert(0, str(Path(__file__).resolve().parent))
import native_v1_ref as ref  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
V1 = ROOT / "contracts" / "v1"


def load(rel: str):
    with open(V1 / rel, encoding="utf-8") as fh:
        return json.load(fh)


def schema_ok(schema_doc, doc) -> bool:
    try:
        jsonschema.validate(doc, schema_doc)
        return True
    except jsonschema.ValidationError:
        return False


def schema_error_text(schema_doc, doc) -> str:
    errs = sorted(jsonschema.Draft202012Validator(schema_doc).iter_errors(doc),
                  key=lambda e: list(e.absolute_path))
    return json.dumps([{"path": list(e.absolute_path), "msg": e.message[:160]}
                       for e in errs], ensure_ascii=False)


class C01ExecSpecCrossLanguage(unittest.TestCase):
    """T41-C01: same argv/shell/null/empty fixture — schema/Python agree;
    argv empty-string arguments are preserved verbatim."""

    @classmethod
    def setUpClass(cls):
        cls.fixtures = load("fixtures/execspec/cases.json")
        cls.schema = load("exec.schema.json")

    def test_fixture_file_wellformed(self):
        self.assertGreaterEqual(len(self.fixtures["cases"]), 20)
        names = [c["name"] for c in self.fixtures["cases"]]
        self.assertEqual(len(names), len(set(names)))

    def test_all_cases_both_engines_agree(self):
        for case in self.fixtures["cases"]:
            with self.subTest(case=case["name"]):
                doc = case["document"]
                expect_ok = case["expect"] == "ACCEPT"
                self.assertEqual(schema_ok(self.schema, doc), expect_ok,
                                 "jsonschema disagrees with fixture")
                py = ref.validate_execspec(doc)
                self.assertEqual(py["ok"], expect_ok,
                                 f"python reference disagrees: {py.get('error')}")
                if expect_ok:
                    norm = py["normalized"]
                    want = case["normalized"]
                    if "argv" in want:
                        self.assertEqual(norm["command"]["argv"], want["argv"])
                        # empty-string argument preserved at its position
                        for got, exp in zip(norm["command"]["argv"], want["argv"]):
                            self.assertEqual(got, exp)
                    if "executable" in want:
                        self.assertEqual(norm["command"]["executable"], want["executable"])
                        self.assertEqual(norm["command"]["script"], want["script"])
                    self.assertEqual(norm["env"], want.get("env", {}))
                    if "cwd" in want:
                        self.assertEqual(norm.get("cwd"), want["cwd"])


class C02C05EnvironmentBoundary(unittest.TestCase):
    """T41-C02: Harbor verifier/reward rejected loudly from the native spec.
    T41-C05: advanced volume/network fields rejected with the field named."""

    @classmethod
    def setUpClass(cls):
        cls.fixtures = load("fixtures/environment/cases.json")
        cls.schema = load("environment.schema.json")

    def test_cases(self):
        for case in self.fixtures["cases"]:
            with self.subTest(case=case["name"]):
                doc = case["document"]
                if case["expect"] == "METADATA_ROUNDTRIP":
                    # adapter-side metadata round-trips exactly; never validated as native
                    clone = json.loads(json.dumps(doc))
                    self.assertEqual(clone, doc)
                    continue
                expect_ok = case["expect"] == "ACCEPT"
                self.assertEqual(schema_ok(self.schema, doc), expect_ok,
                                 "jsonschema disagrees with fixture")
                py = ref.validate_environmentspec(doc)
                self.assertEqual(py["ok"], expect_ok,
                                 f"python reference disagrees: {py.get('error')}")
                if expect_ok:
                    continue
                err_text = schema_error_text(self.schema, doc)
                for field in case.get("expect_fields", []):
                    self.assertIn(field, err_text + json.dumps(py.get("error", "")),
                                  f"rejection must name the offending field {field!r}")


class C03StateMachines(unittest.TestCase):
    """T41-C03: transitions enumerated from states.json; nonzero exit is not
    an operation failure; unknown moves only with evidence."""

    @classmethod
    def setUpClass(cls):
        cls.states = load("states.json")
        cls.fixtures = load("fixtures/states/cases.json")

    def test_every_illegal_fixture_is_not_a_legal_edge(self):
        for case in self.fixtures["illegal_transitions"]:
            with self.subTest(**case):
                legal = self.states["domains"][case["domain"]][case["from"]]
                self.assertNotIn(case["to"], legal)
                self.assertFalse(ref.legal_transition(self.states, case["domain"],
                                                      case["from"], case["to"]))

    def test_all_declared_edges_are_legal(self):
        for domain, table in self.states["domains"].items():
            for src, dsts in table.items():
                for dst in dsts:
                    self.assertIn(dst, table,
                                  f"{domain}: {src}->{dst} targets undeclared state")

    def test_late_confirmation_rules(self):
        late = self.states["late_confirmation"]
        for rule in self.fixtures["late_confirmation_rules"]:
            with self.subTest(domain=rule["domain"]):
                entry = late[rule["domain"]]
                self.assertEqual(entry["from"], "unknown")
                self.assertEqual(sorted(entry["to"]), sorted(rule["allowed_to"]))
                for bad in rule["forbidden_to"]:
                    self.assertNotIn(bad, entry["to"])
                if rule["requires_non_empty"]:
                    self.assertTrue(entry["requires"].strip())

    def test_semantic_facts(self):
        for fact in self.fixtures["semantic_facts"]:
            with self.subTest(fact=fact["name"]):
                self.assertEqual(ref.check_semantic_fact(fact["document"]),
                                 fact["consistent"])


class C04IdentityGenerationRevision(unittest.TestCase):
    """T41-C04: generation fences incarnations; revision is CAS; API restart
    does not increment generation."""

    @classmethod
    def setUpClass(cls):
        cls.fixtures = load("fixtures/identity/cases.json")
        cls.error_schema = load("error.schema.json")

    def test_cases(self):
        for case in self.fixtures["cases"]:
            with self.subTest(case=case["name"]):
                given = case["given"]
                ident = ref.SandboxIdentity(given["sandbox_id"], given["generation"],
                                            given["revision"])
                result = ref.apply_action(ident, case["action"])
                want = case["expect"]
                if "generation" in want and "outcome" not in want:
                    self.assertEqual(result["generation"], want["generation"])
                    self.assertEqual(result["revision"], want["revision"])
                    continue
                self.assertEqual(result["outcome"], want["outcome"])
                if "revision" in want:
                    self.assertEqual(result["revision"], want["revision"])
                if "error" in want:
                    self.assertEqual(result["error"]["code"], want["error"]["code"])
                if "error_document" in want:
                    jsonschema.validate(want["error_document"], self.error_schema)


class CapabilityResolution(unittest.TestCase):
    """Round-02 rejects: single-ID+multi_uid, no-cgroup+hard-mem,
    metadata-proc+private, no-seccomp+required, net-none+outbound; plus
    degradation visibility and unsupported-cannot-disappear."""

    @classmethod
    def setUpClass(cls):
        cls.fixtures = load("fixtures/resolution/cases.json")
        cls.rules = load("resolution-rules.json")
        cls.host_schema = load("host-capabilities.schema.json")
        cls.spec_schema = load("sandbox-requirements.schema.json")

    def test_all_host_profiles_validate(self):
        for name, host in self.fixtures["hosts"].items():
            with self.subTest(host=name):
                jsonschema.validate(host, self.host_schema)

    def test_resolution_cases(self):
        for case in self.fixtures["cases"]:
            with self.subTest(case=case["name"]):
                host = self.fixtures["hosts"][case["host"]]
                spec = case["spec"]
                jsonschema.validate(spec, self.spec_schema)
                got = ref.resolve(host, spec, self.rules)
                want = case["expect"]
                self.assertEqual(got["outcome"], want["outcome"])
                want_rej = sorted((r["requirement"], r["code"], r["capability"])
                                  for r in want.get("rejections", []))
                got_rej = sorted((r["requirement"], r["code"], r["capability"])
                                 for r in got["rejections"])
                self.assertEqual(got_rej, want_rej)
                for degr in want.get("degradations", []):
                    self.assertIn(degr, got["degradations"])
                # unsupported cannot silently disappear: effective enumerates dims
                for dim in ("identity", "process", "resource", "filesystem",
                            "network", "runtime"):
                    self.assertIn(dim, got["effective"])
                # rejection codes come from the frozen rules table only
                for r in got["rejections"]:
                    self.assertIn(r["code"], ("HOST_CAPABILITY_UNAVAILABLE",
                                              "POLICY_UNSUPPORTED"))

    def test_schema_level_cases(self):
        for case in self.fixtures["schema_level_cases"]:
            with self.subTest(case=case["name"]):
                ok = schema_ok(self.spec_schema, case["spec"])
                if case["expect"] == "SCHEMA_REJECT":
                    self.assertFalse(ok)
                else:
                    self.assertTrue(ok)
                    norm = ref.normalize_spec(case["spec"])
                    if "normalized_policy" in case:
                        self.assertEqual(norm["single_id_compatibility_policy"],
                                         case["normalized_policy"])


if __name__ == "__main__":
    unittest.main()
