+++
title = "MCP"
weight = 7
[extra]
group = "Reference"
+++

# MCP (Model Context Protocol)

Maki connects to external tool servers over MCP. Both **stdio** and **HTTP** transports are supported.

## Configuration

Add servers under `[mcp.*]` in your MCP config:

- **Global**: `~/.config/maki/mcp.toml`
- **Project**: `.maki/mcp.toml` in the active Git checkout, or in the working
  directory outside Git (project config wins when both set a value)

Servers in the project file start only after you trust that folder, see
[Folder Trust](/docs/folder-trust/).

### Stdio

```toml
[mcp.filesystem]
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp.github]
command = ["gh", "mcp-server"]
environment = { GITHUB_TOKEN = "ghp_xxxx" }
timeout = 10000
enabled = false
```

### HTTP

```toml
[mcp.analytics]
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${ANALYTICS_TOKEN}" }
```

`headers` and `environment` values expand `${VAR}` from the environment. A
referenced variable that is unset or empty fails that server with the variable
named in its status, instead of sending a dangling `Bearer ` and getting a 401.

Maki does not pass provider API keys such as `ANTHROPIC_API_KEY` to the
processes it starts, so a stdio server that needs one gets it through
`environment = { ANTHROPIC_API_KEY = "${ANTHROPIC_API_KEY}" }`.

Some HTTP servers need OAuth but have no dynamic client registration. For those, give Maki a static client:

```toml
[mcp.acme]
url = "https://mcp.acme.example.com/mcp"
oauth = { client_id = "acme-client", client_secret = "s3cret", callback_port = 3118, callback_path = "/callback", callback_hostname = "localhost" }
```

For a server whose certificate comes from a private CA, point `ca_file` at a PEM bundle:

```toml
[mcp.internal]
url = "https://mcp.corp.example.com/mcp"
ca_file = "~/certs/corp-ca.pem"
```

Maki checks certificates against the operating system's trust store, so a CA that is already trusted in the macOS keychain or the Windows certificate store works without `ca_file`. Set `ca_file` for a CA that is not in that store. The bundle replaces the default CAs for this server and its OAuth endpoints. If OAuth goes through a public provider, add the public CAs to the bundle too, for example `cat /etc/ssl/cert.pem corp-ca.pem > bundle.pem`. The path expands `${VAR}` and `~`, and a relative path starts from the folder of the `mcp.toml` that sets it. On Linux, you can instead trust a CA for every HTTPS request Maki makes by pointing `SSL_CERT_FILE` at a bundle that holds the public CAs as well. On macOS and Windows, MCP servers ignore `SSL_CERT_FILE`, so use `ca_file` there.

### All options

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `command` | array | | Stdio: program + args |
| `url` | string | | HTTP: server URL |
| `environment` | map | | Stdio only. Values expand `${VAR}` from the environment |
| `headers` | map | | HTTP only. Values expand `${VAR}` from the environment |
| `oauth` | table | | HTTP only: OAuth client (optional `client_id`, optional `client_secret`, optional `callback_port`, optional `callback_path`, optional `callback_hostname`, optional `callback_scheme`) |
| `ca_file` | path | | HTTP only. PEM bundle that replaces the default CAs for this server and its OAuth |
| `description` | string | | Summary of the server capabilities shown to the model |
| `timeout` | u64 | 30000 | Milliseconds (1-300000) |
| `enabled` | bool | true | |

Set `command` for stdio, `url` for HTTP. Pick one.

## External libraries and code execution

Every tool definition a server exposes costs context window space on every request. A server with dozens of tools can consume thousands of tokens before a conversation even starts. Adding or loading tool definitions mid-session also alters the request prefix, invalidating the prompt cache and triggering costly cache writes.

To keep the prompt cache completely stable across all models, Maki treats MCP servers as **external Python libraries** called exclusively inside `code_execution`.

MCP functions never enter the top-level API `tools` array. The model sees available modules and their functions listed in the `code_execution` environment description, and can call them via module attributes:

```python
await github.create_issue(title="Bug fix", body="Details")
```

Flat namespaced names (`await github__create_issue(...)`) remain supported as aliases.

```
MCP servers (e.g. github, datadog)
        │
        ▼
context keeps tools array static: [read, write, edit, bash, code_execution, library_help]
        │
        ▼  model calls library_help(library_name="github", function_name="create_issue")
gets Python signature + docstring
        │
        ▼  model writes Python script in code_execution
await github.create_issue(title="Bug fix")
```

### Inspecting function documentation

When at least one MCP server is configured, Maki exposes the `library_help` tool to the model. The tool takes both `library_name` and `function_name` and returns the exact Python async function signature, parameter types, and docstring:

```python
async def create_issue(
    owner: str,
    repo: str,
    title: str,
    body: str = None,
    labels: list[str] = None
) -> str:
    """
    Create an issue in a repository.

    Parameters:
    - owner (str, required): The account owner of the repository.
    - repo (str, required): The name of the repository.
    - title (str, required): The issue title.
    - body (str, optional): The issue contents.
    - labels (list[str], optional): Labels to associate with this issue.
    """
```

When no MCP servers are configured, `library_help` is automatically hidden from the model.

## Naming and namespacing

Server names are ASCII alphanumeric, hyphens ok (no dots). Tools get prefixed with their server name: a `read` tool on the `filesystem` server becomes `filesystem__read`. Because of this, `__` is reserved and names can't collide with built-in tools.

Permission rules for MCP tools use the same nested form under `[mcp.<server>]` in `permissions.toml`. See [Permissions](/docs/permissions/#mcp-tool-permissions).

## Runtime toggling

Open the MCP picker with `/mcp`. Turn servers on or off there; changes save back to your config (project or global, depending on which file defined the server).

## Status

| Status | Meaning |
|--------|---------|
| Connecting | Waiting for the server to come up |
| Running | Tools available |
| Disabled | Off in config or toggled off in UI |
| Failed | Error shown in UI |
| NeedsAuth | Waiting for OAuth (see below) |

If one server fails, the rest still work.

## OAuth

Some HTTP servers need auth. When that happens, Maki opens your browser to log in. Other servers keep working while you authenticate. Tokens refresh on their own. If you change the server URL, you log in again.

```bash
maki mcp auth <server-name>     # manually trigger auth
maki mcp logout <server-name>   # remove stored tokens
```

Servers without dynamic client registration need a client you registered yourself (e.g. your own app on their platform). Add it to the server config so the auth flow uses it instead of trying to register. Without `client_id`, the `callback_*` fields still apply and Maki registers a client dynamically with that redirect URI:

| Field | Type | Notes |
|-------|------|-------|
| `client_id` | string | Optional, client id of your registered app |
| `client_secret` | string | Optional, for confidential clients (needs `client_id`) |
| `callback_port` | u16 | Optional, pins the loopback port so the redirect URI can be pre-registered |
| `callback_path` | string | Optional, loopback path of the redirect URI (default `/mcp/oauth/callback`) |
| `callback_hostname` | string | Optional, loopback hostname of the redirect URI (default `127.0.0.1`) |
| `callback_scheme` | string | Optional, `http` (default) or `https` |

Set `callback_port` when the server only accepts exact redirect URIs. Otherwise Maki falls back to its default port, then to any free port, so the redirect URI changes between runs. Set `callback_path` when the server registered a different path (e.g. `/callback`). Set `callback_hostname` to `localhost` when the server registered the name form instead of the IP (the listener still binds to 127.0.0.1).

Set `callback_scheme = "https"` (needs `callback_port`) when the server only accepts https redirect URIs, such as `https://localhost:3119/callback`. Maki serves the callback with a self-signed certificate it makes for each login, so the browser warns once before the redirect lands. Proceed past the warning and the login finishes, in the TUI or in `maki mcp auth <server-name>`.

### Headless machines

On a machine without a browser (say, a dev server over SSH), run `maki mcp auth <server-name>`. Maki prints the login URL. Open it on your laptop and log in. The browser lands on a `http://127.0.0.1:19876/...` page that fails to load. Copy that full URL from the address bar and paste it into the terminal to finish the login.

## Prompts

MCP servers can expose prompts (reusable message templates). Maki shows them as slash commands in the command palette: `/server:prompt-name`. Type `/` to filter.

```
/github:create-pr           # no arguments
/analytics:report monthly   # one argument
/review:code src tests      # multiple, positional
```

Skip a required argument and Maki shows a usage hint. Prompts are fetched at startup and on reconnect, so new ones need a restart. Only text content is supported.
