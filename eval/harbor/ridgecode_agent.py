"""Install and run a pinned RidgeCode binary inside a Harbor task container.

This adapter intentionally delegates completion scoring to Harbor's task verifier.
RidgeCode's internal ``approved`` value is retained only in the JSONL trace and is
never translated into a Harbor reward.

Required agent environment variables:
  RIDGECODE_BINARY_URL     HTTPS URL for the exact Linux ridgecode binary.
  RIDGECODE_BINARY_SHA256  SHA-256 for that binary (lower-case hexadecimal).
  RIDGECODE_API_KEY        Provider credential, injected as a Harbor secret.

Optional runtime variables mirror RidgeCode's documented machine-run settings:
  RIDGECODE_PROVIDER, RIDGECODE_MODEL, RIDGECODE_BASE_URL,
  RIDGECODE_EFFORT (default: high), RIDGECODE_MAX_TURNS (default: 12),
  RIDGECODE_TIMEOUT (default: 8h), RIDGECODE_BUDGET_TOKENS (default: 0).
"""

from __future__ import annotations

import base64
import os
import re
import shlex
from pathlib import PurePosixPath
try:
    from typing import override
except ImportError:  # Python 3.11 and older Harbor images
    try:
        from typing_extensions import override
    except ImportError:
        def override(function):
            return function

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


_BINARY = "/usr/local/bin/ridgecode"
_TASK_FILE = "/tmp/ridgecode-task.md"
_TRACE_FILE = "/tmp/ridgecode-trace.jsonl"
_SHA256 = re.compile(r"^[0-9a-f]{64}$")


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} must be supplied to the Harbor agent environment")
    return value


def _positive_int_env(name: str, default: int) -> int:
    raw = os.environ.get(name, str(default))
    try:
        value = int(raw)
    except ValueError as error:
        raise RuntimeError(f"{name} must be an integer") from error
    if value < 1:
        raise RuntimeError(f"{name} must be at least 1")
    return value


def _non_negative_int_env(name: str, default: int) -> int:
    raw = os.environ.get(name, str(default))
    try:
        value = int(raw)
    except ValueError as error:
        raise RuntimeError(f"{name} must be an integer") from error
    if value < 0:
        raise RuntimeError(f"{name} must be non-negative")
    return value


def _task_path() -> str:
    """Return a fixed temp path and reject accidental traversal if it changes."""
    path = PurePosixPath(_TASK_FILE)
    if not path.is_absolute() or ".." in path.parts:
        raise RuntimeError("invalid fixed RidgeCode task path")
    return str(path)


class RidgeCode(BaseInstalledAgent):
    """Harbor installed-agent adapter for RidgeCode's isolated machine runner."""

    @staticmethod
    def name() -> str:
        return "ridgecode"

    @override
    def version(self) -> str | None:
        return os.environ.get("RIDGECODE_AGENT_VERSION", "pinned-binary")

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        url = _required_env("RIDGECODE_BINARY_URL")
        digest = _required_env("RIDGECODE_BINARY_SHA256")
        if not _SHA256.fullmatch(digest):
            raise RuntimeError("RIDGECODE_BINARY_SHA256 must be a lower-case SHA-256 hex digest")
        if not url.startswith("https://"):
            raise RuntimeError("RIDGECODE_BINARY_URL must use HTTPS")

        # The values are passed as environment data, not interpolated into shell source.
        await self.exec_as_root(
            environment,
            command=(
                "set -eu; "
                "command -v curl >/dev/null || (apt-get update && apt-get install -y curl); "
                "curl --fail --location --silent --show-error \"$RIDGECODE_BINARY_URL\" "
                f"--output {_BINARY}; "
                f"printf '%s  %s\\n' \"$RIDGECODE_BINARY_SHA256\" {_BINARY} | sha256sum --check --status; "
                f"chmod 0755 {_BINARY}"
            ),
        )

    @override
    @with_prompt_template
    async def run(
        self, instruction: str, environment: BaseEnvironment, context: AgentContext
    ) -> None:
        del context  # Harbor's verifier, not agent self-report, determines task reward.
        _required_env("RIDGECODE_API_KEY")
        max_turns = _positive_int_env("RIDGECODE_MAX_TURNS", 12)
        budget = _non_negative_int_env("RIDGECODE_BUDGET_TOKENS", 0)
        timeout = os.environ.get("RIDGECODE_TIMEOUT", "8h").strip() or "8h"
        effort = os.environ.get("RIDGECODE_EFFORT", "high").strip() or "high"
        encoded = base64.b64encode(instruction.encode("utf-8")).decode("ascii")
        task_path = _task_path()

        command = " ".join(
            [
                "set -eu;",
                f"printf '%s' {shlex.quote(encoded)} | base64 --decode > {task_path};",
                _BINARY,
                "run",
                "--task-file",
                task_path,
                "--jsonl",
                "--no-persist",
                "--isolate-runtime",
                "--require-api-key",
                "--effort",
                shlex.quote(effort),
                "--max-turns",
                str(max_turns),
                "--timeout",
                shlex.quote(timeout),
                "--budget-tokens",
                str(budget),
                ">",
                _TRACE_FILE,
            ]
        )
        await self.exec_as_agent(environment, command=command)

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        # Do not synthesize pass/fail from RidgeCode output. Harbor's verifier owns
        # reward; traces remain in the task container's command logs for diagnosis.
        del context
