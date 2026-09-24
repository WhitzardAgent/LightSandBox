"""Harbor import-path entry. Requires a reviewed/pinned Harbor installation.

Target invocation after the native backend is implemented and configured:
  harbor run -p TASK --env lightsandbox_harbor.provider:LightSandboxEnvironment -a oracle

This module never imports the user's Evaluation project or modifies Harbor enums.
"""
try:
    from harbor.environments.base import BaseEnvironment, ExecResult
    from harbor.environments.capabilities import EnvironmentCapabilities, EnvironmentResourceCapabilities
except ImportError as exc:
    raise ImportError("Install the locked Harbor source before loading this optional provider; offline inspection does not require Harbor") from exc

from .provider_core import make_environment_type

LightSandboxEnvironment = make_environment_type(
    BaseEnvironment, EnvironmentCapabilities, EnvironmentResourceCapabilities, ExecResult
)
LightSandboxEnvironment.__module__ = __name__
