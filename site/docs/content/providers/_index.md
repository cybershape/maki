+++
title = "Providers"
weight = 5
[extra]
group = "Reference"
+++

# Providers

Maki talks to LLM providers over their HTTP APIs. Models are split into three tiers: **weak** (cheap and fast), **medium** (balanced), and **strong** (highest capability, highest cost). There is also a **compaction** tier for choosing a dedicated model to summarize context when the conversation grows long.

Open the model picker with `/model` and press `!`, `@`, `#`, or `$` on any row to assign it to strong, medium, weak, or compaction. Press the same key again to remove the assignment. Your overrides are saved to `~/.local/state/maki/model-tiers` and apply across sessions.

## Auth Reloading

Maki re-reads auth from storage and environment variables each time a new agent spawns (`/new`, retry, session load). If you run `maki auth login` in another terminal or change an env var, the next session picks it up without a restart.

You can set multiple API keys in one env var (`ANTHROPIC_API_KEY=sk-1,sk-2,sk-3`). On a rate-limit or auth error, maki switches to the next key right away, with no delay and without spending a retry. Each request walks the pool once before falling back to normal backoff. A plan quota error does not rotate keys, since the quota is per account and the others are just as spent.

## Base URL Overrides

Every provider honors a `<SLUG>_BASE_URL` env var (`anthropic` -> `ANTHROPIC_BASE_URL`, `llama-cpp` -> `LLAMA_CPP_BASE_URL`). Set it to the origin of a proxy or a compatible endpoint and Maki appends the API paths itself:

```sh
ANTHROPIC_BASE_URL=https://my-proxy.internal maki
```

Built-in, plugin and `providers.toml` providers all read it. When several origins are set, the first one wins:

1. An origin the provider's auth hook returns, such as the endpoint a login flow was given.
2. `<SLUG>_BASE_URL`.
3. `base_url` in `providers.toml`.
4. The provider's own default `base_url`.

`ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL` are the same names the official SDKs use, so an existing proxy setup carries over as is. Two exceptions: `OPENAI_BASE_URL` only redirects the platform API, never the ChatGPT Coding Plan backend; `XAI_BASE_URL` only redirects the public API-key endpoint, never the OAuth CLI proxy.

You can also set `base_url` for a built-in provider in `~/.config/maki/providers.toml`. It overrides the built-in default and loses to the env var above:

```toml
[openai]
base_url = "http://xxxx:1234/v1"
```

The built-in provider still owns the slug, so `protocol`, `api_key_env`, `discover_models` and `models` are ignored with a warning. Use a custom slug if you need those.

## Built-in Providers

### Responses API

- **Env var**: `OPENAI_API_KEY`
- **API endpoints**:

Configured via providers.toml

## Model Identifiers

Models are referenced as `provider/model_id`:

```
anthropic/claude-sonnet-4-6
openai/gpt-4.1
xai/grok-4.6
zai/glm-4.7
```

If the model name is unique across providers, the prefix can be omitted.

### Models newer than your Maki version

The tables above list the models Maki curates. Any other id a provider accepts works too: type it into `/model` or pass it to `--model`. The picker also lists what the provider's own model endpoint reports, so same-day releases are selectable there.

For an id no table covers, rates, context window, vision and thinking support come from [models.dev](https://models.dev/), refreshed daily (`maki models --refresh` forces it). Maki reads each field on its own, so a row that lists a price but no context window still leaves the window to the sources below.

Sources rank by how sure they are to describe the exact model you asked for:

1. What the provider's own model endpoint reported this session.
2. A curated row for that id, including its dated snapshots. `claude-sonnet-4-5-20250929` reads the `claude-sonnet-4-5` row.
3. models.dev.
4. A curated row for a close relative, reached by shared prefix. `glm-5.4` falls back to `glm-5` here, and takes its family and tier from it either way.
5. The provider's defaults, with no cost estimate.

A curated row is checked against the provider's own pricing page, so it wins for the id it names. For a relative it loses to models.dev, because a rate nobody checked against the id you typed is only a guess.

New models start at the **medium** tier until you assign one in the picker.

## providers.toml

`providers.toml` lives in the config directory (`~/.config/maki/providers.toml` on Linux/macOS, `%APPDATA%\maki\providers.toml` on Windows). Use it to:

1. Tweak a built-in (pick a plan, change its base URL, set `enable_free_models` for Opencode).
2. Declare a custom provider that speaks OpenAI, Anthropic, or Google wire format.

```toml
# Point a built-in at a proxy. Env vars still win over this file.
[anthropic]
base_url = "https://my-proxy.internal"

# Full custom provider. Slug becomes the `provider/` prefix in model specs.
[my-proxy]
display_name = "My Proxy"
protocol = "openai"            # openai | openai-responses | anthropic | google
base_url = "https://llm.example.com/v1"
api_key_env = "MY_PROXY_API_KEY"
default_model = "my-proxy/fast-v1"
discover_models = true         # also list models via the provider's /models endpoint

[[my-proxy.models]]
id = "fast-v1"
tier = "weak"
context_window = 128000
max_output_tokens = 16384
pricing_input = 0.5
pricing_output = 1.5

[[my-proxy.models]]
id = "smart-v1"
tier = "strong"
context_window = 200000
max_output_tokens = 32000
supports_thinking = true
supports_vision = false
```

### Provider fields

| Field | Type | Notes |
|-------|------|-------|
| `display_name` | string | Shown in pickers and auth status |
| `protocol` | string | `openai`, `openai-responses`, `anthropic`, or `google`. Required for custom slugs |
| `base_url` | string | Origin of the API. Maki appends the protocol paths |
| `plan` | string | Built-in plan key (see Plans below). Sets base URL and default model |
| `api_key_env` | string | Env var that holds the key. Defaults to `<SLUG>_API_KEY` |
| `api_key` | string | Inline key (prefer the env var or `maki auth login`) |
| `headers` | table | Extra HTTP headers sent on every request to this provider. Values expand `${VAR}` from the environment; an unset or empty variable fails the provider instead of sending a half-filled header. A same-name header (case-insensitive) replaces the built-in auth header and survives key rotation |
| `default_model` | string | Used after login when no model is saved yet. On a custom entry it is also the startup fallback when no built-in or plugin provider is available. Without it, startup picks a declared `strong` or `medium` model |
| `top_p` | f64 | Nucleus sampling probability, sent as `top_p` in the request body for OpenAI-compatible, Anthropic, Bedrock and Google providers. Claude and GPT models reject it while thinking is on, and Claude Opus 4.7, Claude 5 and later always do, so it is dropped there. Never sent to Copilot or over the OpenAI responses path. Only sent when set, so the provider's own default applies otherwise. Must be in `(0, 1]` |
| `discover_models` | bool | When true, also probe the provider's model list endpoint (default false) |
| `enable_free_models` | bool | Opencode only. Show free catalog models (default false) |
| `subsidised_by` | string | Name of the flat subscription prepaying this provider (e.g. `"Max"`). Models bill $0 and show the published list price beside it as a reference. The list-price fallback needs `protocol = "anthropic"` |
| `supports_deferred_tools` | bool | The endpoint can load a deferred MCP tool without rewriting the cached tools prefix (see [MCP](../mcp/#loads-and-the-prompt-cache)). True for Anthropic direct and Bedrock. A custom `protocol = "anthropic"` provider, or a built-in pointed at another `base_url`, defaults to false and opts in here |
| `models` | array | Declared models for custom providers (see below) |
| `overrides` | table | Aperture only. Per-upstream model overrides (see below) |

### Model fields

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `id` | string | required | Model id. Spec becomes `{slug}/{id}` |
| `tier` | string | `medium` | `weak`, `medium`, `strong`, or `compaction` |
| `context_window` | u32 | protocol default | Tokens of context |
| `max_output_tokens` | u32 | protocol default | Max completion tokens |
| `supports_tool_examples` | bool | protocol default | |
| `supports_thinking` | bool | protocol default | |
| `requires_thinking` | bool | false | For APIs that reject requests with thinking disabled. Implies `supports_thinking` and raises thinking to minimal effort when off (including compaction). On generic `openai` entries without `thinking_fields` its only wire effect is that the `disabled` block is never sent |
| `thinking_fields` | table | unset | How this model spells each thinking mode on the wire. The only thinking control on generic `openai` entries, and it implies `supports_thinking` |
| `supports_vision` | bool | protocol default | When false, image input and `view_image` are off |
| `pricing_input` / `pricing_output` | f64 | 0 | USD per 1M tokens |
| `pricing_cache_write` / `pricing_cache_read` | f64 | 0 | USD per 1M tokens |
| `pricing_fast_input` / `pricing_fast_output` | f64 | unset | Fast-mode pricing when the provider supports it |

Custom slugs must not reuse a built-in provider name. Maki exits with code 2 at startup if `providers.toml` does not parse or contains an unknown key, so a typo like `api_key_evn` fails there rather than being silently ignored.

Custom `openai`-protocol models control thinking with `thinking_fields`. Each key is a thinking mode, and its JSON fragment merges into the request body. An effort level without a key uses the closest declared level below it, or the lowest declared level if none is below. `off` and `adaptive` need their own keys and never fall back:

```toml
[[my-ollama.models]]
id = "qwen3.8-coder-27b-mlx:latest"
supports_thinking = true

[my-ollama.models.thinking_fields]
off = { reasoning_effort = "none" }
adaptive = { reasoning_effort = "medium" }
low = { reasoning_effort = "low" }
medium = { reasoning_effort = "medium" }
high = { reasoning_effort = "xhigh" }
max = { reasoning_effort = "xhigh" }
```

A mode you left out sends nothing. The built-in `ollama` slug needs no thinking keys, because it asks Ollama which levels each model takes (see [Ollama](#ollama)). To override that for a model, set `[ollama].base_url` (or `OLLAMA_HOST`) and give `[[ollama.models]]` the thinking keys. Only `supports_thinking`, `requires_thinking` and `thinking_fields` overlay onto a built-in slug. The rest of the entry stays ignored, and startup names the keys it dropped.

Without `thinking_fields`, maki sends `"thinking": {"type": "disabled"}` when thinking is off and nothing when it is on. GLM and Kimi gateways need this block to stop reasoning. Declaring `thinking_fields` replaces it, so add an `off` key if your gateway needs one. Some gateways, such as LiteLLM in front of Azure, reject the block. Maki then retries without it, and if the retry works, leaves the block out for that model until restart.

You can also create a custom provider interactively with `maki auth login` and choosing the custom option. That writes a starter entry to this file.

### Aperture overrides

Aperture proxies upstream providers, exposing each model as `aperture/<upstream>/<model>`. Overrides keyed by upstream provider id live under `[aperture.overrides]`:

```toml
[aperture.overrides.llmserver]
base = "llama-cpp"
context_window = 131072
max_output_tokens = 16384

[aperture.overrides.llmserver.models."qwen-3.6"]
context_window = 262144
supports_vision = true
```

Provider-level fields apply to every model from that upstream; per-model entries under `models` win field by field. Fields: `context_window`, `max_output_tokens`, `supports_thinking`, `supports_vision`, `base` (remaps an opaque vendor to a native provider; e.g. `llama-cpp`, `google`, `anthropic`), and `path_prefix`. Model ids containing dots must be quoted (`"qwen3.6"`) since TOML treats a bare dotted key as a nested table.

Maki sends `/v1` (or `/v1beta` for Gemini routes, nothing for Anthropic and Z.AI), and Aperture appends that path to the upstream's base url. If an upstream base url already carries its own path, set `path_prefix = ""` for it to avoid a doubled path. Z.AI defaults to no prefix since its API path has no `/v1` segment; point the upstream base url at the full API root (e.g. `https://api.z.ai/api/paas/v4`).

### Plans

No built-in currently ships more than one plan.

## Plugin Providers

A [Lua plugin](/docs/plugins/) can add a provider. Call `maki.provider.register` at the top level of the plugin file, and its models become `{slug}/{model_id}` (e.g. `acme/acme-large`) in `/model` and the picker. They get the same retries, pricing and usage accounting as a built-in provider.

```lua
maki.provider.register({
  slug = "acme",
  display_name = "Acme",
  codec = "openai",
  base_url = "https://api.acme.com/v1",
  api_key_env = "ACME_API_KEY",
  models = {
    { prefixes = { "acme-large" }, tier = "strong", context_window = 200000 },
  },
})
```

The plugin's `plugin.toml` must grant `net`, list the provider's hosts in `net_hosts`, and list the slug under `providers` when models.dev serves that name. `api_key_env` also needs `env`:

```toml
providers = ["acme"]

[permissions]
net = true
env = true
net_hosts = ["api.acme.com"]
```

Maki sends the provider's credentials only to those hosts, plus any origin you set yourself with `<SLUG>_BASE_URL` or `providers.toml`. The `base_url` must be `https`, or `http` on loopback. See [plugin egress](/docs/permissions/#plugin-egress-net-hosts) for the pattern syntax.

The [Lua API](/docs/lua-api/#maki-provider-register) lists every field. This section explains the choices.

### Granted slugs

A third-party plugin that registers a slug models.dev serves has to be granted it: the manifest lists the slug at the top level of its `plugin.toml`, and the install approval covers it. The install approval prompt shows that the package will receive your saved key for each name models.dev serves.

```toml
providers = ["acme"]

[permissions]
net = true
net_hosts = ["api.acme.com"]
```

Maki checks the list against the names models.dev serves today. A load that registers such a slug the manifest does not list, or the approval never covered, is refused. List every slug the package registers, so a name the catalog adds later is already approved and the generic catalog client can never take over a working package's slug and receive the key you saved for it.

Names Maki ships, built in or as a bundled plugin, stay [reserved](#slug-rules) regardless of this list.

An approval recorded before this list existed covers no slug. The startup approval check asks again, so update or reinstall the package and answer the prompt to grant the names.

This replaces the `plugin = "..."` selection field an older release had in `providers.toml`. Maki still reads it: while it is set, the slug stays owned by the named package so the catalog client cannot take it over, and a warning tells you to list the slug under `providers` in the package's `plugin.toml` and remove the `plugin =` line.

### codec or base

Set exactly one of the two. `codec` picks the wire format the API speaks:

| `codec` | Wire format |
|---------|-------------|
| `openai` | OpenAI chat completions |
| `openai-responses` | OpenAI responses API |
| `anthropic` | Anthropic messages |
| `google` | Gemini `generateContent` |

`base` borrows a built-in provider's whole adapter, quirks included, such as Ollama's thinking field or Copilot's endpoint routing. Use it when porting a provider script that set `base`, or when no codec fits. A base changes whenever that provider does, so prefer a codec. Valid values: .

Without a `list_models` hook, the provider lists what its codec or base lists, such as `GET /models` for `codec = "openai"`. A `base` also lends its model rows, so a model your `models` table leaves out keeps the base's price, limits and tier.

`family`, `accepts_arbitrary_models`, `max_output_tokens` and `context_window` apply to models without a row, and default to the provider behind the codec or base. `codec = "openai"` defaults to the `gpt` family, so set `family = "generic"` unless the API serves GPT models. Rows that leave out a limit take the provider's.

### Model rows

`models` is read once at registration. Rows describe models, and the picker lists them next to the runtime list. To change how the runtime list is fetched, use the `list_models` hook.

A row matches every model id that starts with one of its `prefixes`, and the longest match wins: `acme-large-2504` uses an `acme-large` row over an `acme` row. `prefixes[1]` is the canonical id, shown in the picker and used in `{slug}/{model_id}`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `prefixes` | list of strings | required | The first is the canonical id |
| `tier` | string | `medium` | `weak`, `medium`, `strong`, or `compaction` |
| `context_window` | number | provider's | Tokens of context |
| `max_output_tokens` | number | provider's | Max completion tokens |
| `supports_thinking` | bool | unset | |
| `requires_thinking` | bool | `false` | For APIs that reject a request with thinking off. Implies `supports_thinking` and raises thinking to minimal effort when off |
| `supports_vision` | bool | unset | When false, image input and `view_image` are off for this model |
| `supports_tool_examples` | bool | unset | |
| `pricing` | table | unset | `input`, `output`, `cache_write`, `cache_read`, in dollars per 1M tokens |
| `thinking_fields` | table | unset | How this model spells each thinking mode on the wire |
| `family` | string | provider's | `generic`, `claude`, `gpt`, `gemini`, `glm` or `synthetic` |
| `default` | bool | first row of its tier | The tier's default model. At most one per tier |

An unset `supports_*` flag uses the codec or base provider's answer, and `false` turns the feature off for that model.

`thinking_fields` works as in [providers.toml](#providers-toml). Keys are `off`, `adaptive` and the effort levels `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, each mapped to a JSON fragment merged into the request body. A mode you leave out sends nothing with a codec, and falls back to the built-in mapping with `base = "llama-cpp"` or `base = "ollama"`.

### Hooks

Every hook is optional. Without any, the provider reads its key from `api_key_env`.

| Hook | Runs |
|------|------|
| `auth(ctx, purpose)` | `"resolve"` before the first request, `"refresh"` after a 401 on the newest credentials (then retries), `"reload"` to re-read stored credentials after a login or another maki process changed them |
| `list_models(ctx)` | When the picker or `maki models` lists models |
| `build_body(ctx, body, model, opts)` | On every request, with the final body |
| `map_error(ctx, status, message)` | On an API error, before it reaches the UI |
| `fetch_usage(ctx)` | When the usage display asks for quota |
| `login(ctx)` | `maki auth login <slug>` |
| `logout(ctx)` | `maki auth logout <slug>` |

Each hook gets a [`ctx`](/docs/lua-api/#maki-provider-register) table first, with the slug, the current origin and headers, and a `ctx.get_json` helper. Build URLs from `ctx.base_url` so side calls follow a user who points the slug at a gateway. To fail, return `nil, err` with an error from `ctx.get_json` or `maki.provider.http_error`, and Maki retries as it would for a built-in provider.

Credentials resolve on the first request. A provider with missing or expired credentials stays in the picker and fails when you send a message, like a built-in provider with no API key.

`maki auth login` lists a provider that defines `login` or `api_key_env`. With `login`, it runs the hook. Otherwise it asks for one of the `plans` if there are any, opens `login_url`, and saves the key you paste.

`map_error` can change the status and message of an API error, for example to turn an opaque vendor error into advice. Retries follow the new status, and `retry-after` still comes from the server.

`build_body` needs the `openai` or `openai-responses` codec, and the `google` codec refuses `system_prefix`. Both mistakes fail at registration.

### Credentials

`maki.provider.auth` stores credentials for the plugin's own slugs:

```lua
maki.provider.auth.set("acme", { access_token = token, expires_at = when })
local creds = maki.provider.auth.get("acme")
maki.provider.auth.clear("acme")
```

The value is any JSON object. Each slug gets its own file at `~/.local/state/maki/auth/plugins/<slug>.json`, with mode 0600, atomic writes and a lock against other Maki processes. An `auth` hook can call `set` to save a refreshed token.

### Slug rules

- Starts with a letter or digit, then only letters, digits, `_` and `-`
- Not a slug Maki ships, built in or as a bundled plugin, even one you turned off. Your plugin would otherwise get the API key saved for that provider
- Listed under `providers` in the package's `plugin.toml` and covered by its install approval when models.dev serves the slug. List every slug the package registers, so a name models.dev adds later cannot break a working package
- Not a custom provider from `providers.toml` or a slug another plugin registered

### Migrating from provider scripts

Maki no longer runs executable scripts from the config `providers/` directory. At startup it names every script that no plugin has replaced yet. To port them, run:

```bash
maki migrate providers
```

It lists those scripts and prints a prompt that asks a coding agent to port them to Lua plugins. The prompt names your files and directories, maps each part of the script protocol to the Lua API, and ends with checks the agent runs before it reports back. It goes to stdout, so you can start maki with it or copy it into another agent:

```bash
maki "$(maki migrate providers)"
maki migrate providers | pbcopy
```

If a script was your only way to reach a model, the new maki has no model to run the prompt with. Use the binary that `maki update` replaced, which it keeps in the state directory. Releases before 0.5.8 still run scripts, and when the backup is one of them, `maki migrate providers` shows the command:

```bash
~/.local/state/maki/maki_backup "$(maki migrate providers)"
```

With the old binary, the agent also checks that each plugin lists the same models the script did. The next `maki update` overwrites the backup, so port your scripts before you update again. Without a backup, paste the prompt into another agent.

Each plugin keeps its script's file name as the slug, so saved models and `maki auth login <slug>` keep working. The warning for a script stops once a plugin registers its slug. The prompt tells the agent to leave the scripts and their credential files in place, and to tell you which ones you can delete once every check passes.

To port a script by hand, map each subcommand to part of the registration:

| Script subcommand | Lua |
|-------------------|-----|
| `info` | The same fields on the registration table, minus `has_auth`. Defining `login` replaces it |
| `models` | The `models` table. `id = "acme"` becomes `prefixes = { "acme" }` and matches the same ids. Rows no longer bound the list, so if the API has no model list, add `list_models = function() return {} end` to list only the rows |
| `resolve`, `refresh`, `reload` | `auth = function(ctx, purpose)`, with `purpose` naming the subcommand |
| `login` | `login = function(ctx)`, using `ctx.print`, `ctx.prompt` and `ctx.open_url` |
| `logout` | `logout = function(ctx)` |

A script whose `base` was `mistral`, `deepseek`, `openrouter`, `requesty`, `synthetic`, `regolo` or `tensorx` uses `codec = "openai"` now, with that provider's origin as `base_url`. Those providers are Lua plugins themselves, so they cannot be a `base`.

A static `base_url` only works with `codec`. A script that kept its `base` and returned a `base_url` from `resolve` returns it from the `auth` hook now.

A script that kept credentials in its own file can import them on first use, so nobody has to log in again. Call this from a hook, since `maki.provider.auth.set` only works inside one:

```lua
local function credentials()
  local stored = maki.provider.auth.get("acme")
  if stored then
    return stored
  end
  local text = maki.fs.read(maki.fs.normalize("~/.maki/auth/acme.json"))
  if not text then
    return nil
  end
  local imported = maki.json.decode(text)
  maki.provider.auth.set("acme", imported)
  return imported
end
```

### Worked example

Maki's tests load this plugin, so it matches the current API. It uses `codec = "openai"` and every hook.

`plugin.toml`:

```toml
providers = ["acmelua"]

[permissions]
net = true
net_hosts = ["api.acme.example"]
```

`init.lua`:

```lua
-- An OpenAI-compatible provider that uses every `maki.provider.register` hook
-- once. The provider docs quote this file as their worked example.

local ANONYMOUS = "anonymous"
local REFRESH = "refresh"
local MODELS_PATH = "/models"

local function stored_token(slug)
  local stored = maki.provider.auth.get(slug)
  return (stored and stored.token) or ANONYMOUS
end

local function bearer(token)
  return { headers = { authorization = "Bearer " .. token } }
end

maki.provider.register({
  slug = "acmelua",
  display_name = "Acme (Lua)",
  codec = "openai",
  base_url = "https://api.acme.example/v1",
  -- Prepended to maki's system prompt.
  system_prefix = "Acme house rules: answer in full sentences.",
  models = {
    {
      prefixes = { "acme-1", "acme" },
      tier = "strong",
      context_window = 200000,
      max_output_tokens = 8192,
      supports_thinking = true,
      -- The only levels Acme accepts. Maki snaps any other level onto them.
      thinking_fields = {
        low = { reasoning_effort = "low" },
        high = { reasoning_effort = "high" },
      },
    },
  },

  -- `purpose` is "resolve" before the first request, "reload" after a login
  -- in another process changed the store, and "refresh" after a 401. An Acme
  -- token is single use, so a refresh mints and stores the next one here.
  auth = function(ctx, purpose)
    if purpose ~= REFRESH then
      return bearer(stored_token(ctx.slug))
    end
    local renewed = stored_token(ctx.slug) .. "-renewed"
    maki.provider.auth.set(ctx.slug, { token = renewed })
    return bearer(renewed)
  end,

  -- The catalogue changes faster than this file, so the picker asks the API.
  -- `ctx.get_json` uses the chat requests' origin and headers.
  list_models = function(ctx)
    local body, err = ctx.get_json(MODELS_PATH)
    if err then
      return nil, err
    end
    local models = {}
    for _, m in ipairs(body.data or {}) do
      table.insert(models, { id = m.id, context_window = m.context_length, tier = "strong" })
    end
    return models
  end,

  -- Gets the final body, thinking level included. Acme wants the effort under
  -- its own key. `opts.thinking` is nil when thinking is off.
  build_body = function(_, body, model, opts)
    body.acme_reasoning = { model = model, effort = body.reasoning_effort, asked_for = opts.thinking }
    body.reasoning_effort = nil
    return body
  end,

  -- Acme answers 429 for a spent monthly allowance, which no retry can fix,
  -- so a 400 stops the retries.
  map_error = function(_, status, message)
    if status == 429 and message:find("allowance") then
      return { status = 400, message = "Acme allowance is spent until the next cycle" }
    end
  end,

  fetch_usage = function()
    return { plan = "team", limits = { { label = "Monthly allowance", percentage = 42 } } }
  end,

  -- Defining `login` lists the provider in `maki auth login`.
  login = function(ctx)
    local key = ctx.prompt({ label = "Acme API key: ", secret = true })
    if not key or key == "" then
      ctx.print("No key entered, nothing was stored.")
      return
    end
    maki.provider.auth.set(ctx.slug, { token = key })
    ctx.print("Stored your Acme key.")
  end,

  logout = function(ctx)
    maki.provider.auth.clear(ctx.slug)
    ctx.print("Forgot your Acme key.")
  end,
})
```

