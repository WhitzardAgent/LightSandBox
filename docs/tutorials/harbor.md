# Harbor integration

!!! warning "Status: planned integration"
    The Harbor provider does not exist yet in runnable form. This page is a design skeleton: it records the integration's goals and boundaries so integrators can evaluate the direction. There are intentionally **no commands** on this page — nothing here can be run today, in either stock Harbor or this repository.

## Design goals

Harbor is a benchmark harness: it owns tasks, datasets, trials, agents, oracles, verifiers, rewards, and artifact collection. LightSandbox's runtime core owns none of those concepts. The integration keeps it that way through a strict three-layer boundary:

1. **Stock Harbor stays stock.** Task configuration, trial orchestration, agent steps, oracle/verifier logic, and reward computation remain Harbor's, unmodified. LightSandbox never ships a patched Harbor, and compatibility will be demonstrated against upstream, not against a fork.
2. **A LightSandbox Harbor provider adapts.** The provider implements Harbor's environment abstraction (`BaseEnvironment` boundary) by translating Harbor's environment calls — start, exec, file transfer, network, stop — into native v1 operations. It performs only the necessary protocol conversion.
3. **Native environment services stay clean.** Image, process, volume, network, identity, and resource operations carry no agent/verifier/reward semantics. An environment spec contains no verifier configuration and no artifact rules; recipe-level choices (which environment configuration, which main service, build inputs) live in the adapter and are never serialized into runtime core types.

Consequences of the boundary:

- **Per-service truth before multi-service claims.** A compose-style environment is only claimed once per-service exec, copy, and stop genuinely work — including health checks, dependency conditions, service DNS, and volume initial content. Parsing a compose file is not compatibility.
- **Reference-faithful control order.** Verification flows that stop the main service and run sidecar collection must reproduce the upstream control sequence exactly; a stronger-isolation variant is allowed only as a separately labeled mode, never silently substituted.
- **Task content never picks code.** Task paths come from operator-authorized directories; backend/provider import paths are operator-fixed. Nothing in a task may designate a Python module for the runtime to execute.
- **Build inputs stay sealed.** Test/solution material must not leak into agent build contexts — provider priority rules (prebuilt image vs Dockerfile vs force-build) follow the locked provider, not an ad-hoc order.

## What exists today

Design documentation only (internal design docs in the repository under `docs/v5/`, including the Harbor boundary spec). No provider package, no end-to-end run, no upstream interoperability claim. When the provider lands, this page will grow a real quickstart against a pinned Harbor release.

Related: `concepts/architecture.md` for why evaluation logic lives outside the runtime core.
