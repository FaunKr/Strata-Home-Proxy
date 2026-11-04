# Connecting apps

Every app talks to the proxy on the NAS, never to Strata on the PC directly.
Each app gets its own API key from the web UI (*Access* area); copy it right
away, it is not shown again. Without a valid key the proxy answers `401` and
does not wake the PC.

The proxy speaks the OpenAI and the Anthropic format on the same port:

- OpenAI format: `http://<NAS-IP>:8080/v1`
- Anthropic format: `http://<NAS-IP>:8080`

The first request after a long pause can take a few minutes while the PC
wakes up and Strata loads the model. Streamed answers get a ping every 15 s
meanwhile, so apps do not give up.

## opencode

opencode does not ask the proxy how large the model's context is. Without a
`limit` it takes the context as unknown, never compacts on its own and only
compacts after Strata refuses a prompt that does not fit. Give it the limit in
`opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "strata": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Strata",
      "options": { "baseURL": "http://<NAS-IP>:8080/v1", "apiKey": "<API key from Access>" },
      "models": {
        "strata": { "name": "Strata", "limit": { "context": 65536, "output": 16384 } }
      }
    }
  },
  "model": "strata/strata"
}
```

- `limit.context`: the model's context (*Strata* area, tab *Models*). If the
  proxy switches models behind one name (*Hide model name*), the smallest
  context among them.
- `limit.output`: a quarter of the context, at most 32000. opencode compacts
  at context minus output and caps output at 32000 anyway. The code benchmark
  writes the same rule.
- The model name can be the public model name: Strata serves the model it
  runs whatever the name.

## Claude Code

```sh
export ANTHROPIC_BASE_URL=http://<NAS-IP>:8080
export ANTHROPIC_API_KEY=<API key from Access>
```

Claude Code sends the key as `x-api-key`.

## Open WebUI

*Admin Settings -> Connections -> OpenAI API*: URL `http://<NAS-IP>:8080/v1`,
and as key the API key from *Access*.

## The skill local-agent

[tools/local-agent/README.md](../tools/local-agent/README.md): it needs its
own key from *Access* and the Strata settings named there. `local-agent check`
tells you whether everything is in place.

## Other apps

Any app that talks to an OpenAI- or Anthropic-compatible API works the same
way: the address above and an API key from *Access*. Apps that can read
`x-strata-proxy-status` learn why a request was turned away, for example
`gaming` while gaming mode is on ([status header](how-it-works.md#status-header)).
