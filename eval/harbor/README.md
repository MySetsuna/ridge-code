# RidgeCode Harbor adapter

`ridgecode_agent.py` is a local Harbor `BaseInstalledAgent` integration. It
downloads one exact Linux binary, verifies its SHA-256, then starts
`ridgecode run` in the task container's agent account. The Harbor task verifier
is the only source of reward; RidgeCode's internal `approved` field is not
mapped to a pass.

Supply these values through Harbor's secret/environment mechanism, never in a
checked-in job configuration:

```text
RIDGECODE_BINARY_URL=https://.../ridgecode-linux-x86_64
RIDGECODE_BINARY_SHA256=<64 lower-case hex characters>
RIDGECODE_API_KEY=<provider key>
RIDGECODE_PROVIDER=openai
RIDGECODE_MODEL=glm-5.3
RIDGECODE_BASE_URL=https://ark.cn-beijing.volces.com/api/plan/v3
```

`RIDGECODE_EFFORT`, `RIDGECODE_MAX_TURNS`, `RIDGECODE_TIMEOUT`, and
`RIDGECODE_BUDGET_TOKENS` are optional and are recorded by the surrounding
Harbor job configuration. Pin the binary URL and digest, dataset version,
model, effort, timeout, attempts, and resource envelope in every comparison.

With Harbor installed, run a smoke dataset with the adapter path:

```bash
harbor run -d "<dataset@version>" --agent eval.harbor.ridgecode_agent:RidgeCode
```

Harbor must be launched from the repository root so `eval.harbor` is importable.
Use a Docker or remote Harbor environment only after its executor is healthy;
the adapter itself never installs Docker or changes host state.

On a Windows host, run the read-only preflight before starting a standard
benchmark. It checks the adapter, Harbor, Docker engine, and free storage but
does not install anything or read provider credentials:

```powershell
.\scripts\harbor-preflight.ps1 -StoragePath C:\code\ridge-code
```
