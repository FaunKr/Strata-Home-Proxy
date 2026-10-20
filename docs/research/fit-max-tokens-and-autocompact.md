# `fit_max_tokens` and Claude Code auto-compact against Strata

Research note, 2026-10-19. Sources: Strata at commit `477ac13` (the commit
`docs/strata-api.md:5` pins), the Claude Code docs at code.claude.com/docs as
fetched on 2026-10-19 (changelog up to v2.1.300), and this repo.
**[Inference]** marks conclusions that no source states directly.

Strata links below use `S` = `https://github.com/Niko1221/Strata/blob/477ac13`.

## 1. What `"fit_max_tokens": true` does

### What the source says

- **Where it is set.** You can set it as the key `"fit_max_tokens": true` in
  `strata-<model>.json`, or pass the flag `--fit-max-tokens` to
  `serve/server.py`. The key must be the JSON literal `true`:
  `fit_max_tokens=a.fit_max_tokens or cfg.get("fit_max_tokens") is True`
  ([S/serve/server.py#L6950](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L6950)).
  The flag's help text says: "clamp max_tokens to the remaining context instead
  of rejecting the request (default: reject with 400, like llama.cpp …)"
  ([#L6819-L6821](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L6819-L6821)).
  The key is read when Strata starts. The web page's model settings card can
  edit it, and the change applies "from its next start"
  ([S/docs/DETAILS.md#L991-L996](https://github.com/Niko1221/Strata/blob/477ac13/docs/DETAILS.md#L991-L996)).
- **How the room is computed.** In `Service.prepare`, room is
  `room = ctx - CTX_SLACK - len(ids)`, with `CTX_SLACK = 8`
  ([#L4022](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4022),
  [#L217](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L217)).
  The rules that follow
  ([#L4023-L4040](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4023-L4040)):
  - If `max_tokens` is missing or `<= 0`, the request gets the whole room. This
    holds whether the option is on or off.
  - If `max_tokens > room` and the option is **off**, Strata returns
    **400** `request exceeds the context window: prompt (N tokens) + max tokens
    (M) exceeds the context (C); requests are never truncated. Send a smaller
    max_tokens (at most R here), or add "fit_max_tokens": true …`.
  - If `max_tokens > room` and the option is **on**, Strata sets
    `max_new = max(1, room)`, with the comment "a shorter completion beats a 400".
  - If the prompt alone leaves no room (`room < 1`), Strata still returns 400
    with the option on: "prompt … leaves no room to answer … requests are never
    truncated".
- **Only the output cap changes, never the input.** "Requests longer than that
  are refused, never silently cut. … agents that always ask for their full
  output cap can instead get it shortened to the room left … A prompt that
  leaves no room at all is still refused"
  ([S/docs/DETAILS.md#L984-L987](https://github.com/Niko1221/Strata/blob/477ac13/docs/DETAILS.md#L984-L987)).
- **The clamp is silent.** The branch at
  [#L4040](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4040)
  prints no log line, and the response has no field or header that reports the
  reduced cap. The only trace is `usage.output_tokens` together with
  `stop_reason`.
- **How the clamped cap reaches the Anthropic endpoint.** `_anthropic` reads
  `max_tokens` from the request ("0/-1: the rest of the context") and passes
  the clamped `max_new` that `prepare` returns on to generation
  ([#L6181-L6189](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L6181-L6189)).
- **What the client sees when the clamped cap is hit.** The finish reason is
  `"length"`, which becomes `stop_reason: "max_tokens"`
  ([#L5046-L5048](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L5046-L5048)).
  `stop_reason` is `tool_use` only if every streamed call finished and
  `finish == "stop"`. Strata opens a tool call as a `tool_use` block and
  streams its arguments as `input_json_delta` pieces
  ([#L5008-L5020](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L5008-L5020)).
  A cap hit in the middle of a call therefore leaves a started `tool_use`
  block with **partial JSON**, and the message ends with
  `stop_reason: "max_tokens"`.
- **Interaction with thinking.** The `max_new` that `prepare` returns is
  already clamped, and the thinking limits work from it:
  - With `answer_room` on (the default), `max_new >= 1024` and no budget,
    thinking is closed early to leave the answer
    `min(max(512, max_new//4), max_new//2)` tokens
    ([#L4199-L4207](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4199-L4207),
    [#L150-L151](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L150-L151)).
  - An explicit budget is cut down the same way
    ([#L4208-L4215](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4208-L4215)).
  - Below 1024 no thinking limit applies. Strata then logs that "the reply
    reached max tokens while still thinking, so it has no answer"
    ([#L4615-L4618](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4615-L4618)).
- **Error format.** The 400 for a request that is too long goes out as
  `{"error": {"type": "invalid_request_error", "message": …}}`
  ([#L5655-L5660](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L5655-L5660)).
  Its wording starts with "request exceeds the context window". It is not
  Anthropic's "prompt is too long" wording.
- **Strata recommends it for agents.** For OpenCode, Strata's docs say: "Keep
  `limit.output` well under it: a request whose prompt plus `max_tokens` runs
  past the context is refused … or add `"fit_max_tokens": true`"
  ([S/docs/DETAILS.md#L976-L979](https://github.com/Niko1221/Strata/blob/477ac13/docs/DETAILS.md#L976-L979)).

### How the proxy handles it

The proxy does nothing for `fit_max_tokens`:

- It reads the body only to check `stream` (`crates/nas-proxy/src/proxy.rs:113`).
- It forwards the body unchanged (`proxy.rs:237`).
- It passes Strata's status and body through unchanged and only adds SSE pings
  (`proxy.rs:281-305`).

A 400 from Strata therefore reaches Claude Code exactly as Strata sent it. The
repo tells you to turn the option on for `local-agent`: "`fit_max_tokens` on,
so that Strata trims `max_tokens` instead of refusing with 400"
(`tools/local-agent/README.md:32`; also `docs/setup.md:116` and
`/mnt/project-files/plaene/local-agent.md:27`).

### Upsides

1. **No 400 just because Claude Code asks for its full output cap.**
   Claude Code sends a fixed `max_tokens` on most requests:
   - `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, or the model's default.
   - For a model ID it can't resolve, the default is 32000
     ([env-vars](https://code.claude.com/docs/en/env-vars), `CLAUDE_CODE_MAX_OUTPUT_TOKENS`).

   With a 64K window, a 25K-token Claude Code prompt
   (`tools/local-agent/local-agent:45`) plus 32000 already leaves little
   headroom. Without the option, every request whose prompt is above
   `ctx − 8 − max_tokens` fails, even though the actual answer would have been
   short.
2. **It covers what Claude Code's own recovery may not.** Claude Code retries a
   request "rejected because the input plus `max_tokens` exceeds the context
   limit" with a smaller `max_tokens`
   ([errors, Automatic retries](https://code.claude.com/docs/en/errors#automatic-retries)).
   It recovers from too-long prompts by recognizing specific wordings ("Prompt
   is too long", Bedrock's "Input is too long…", `capability_rejected:
   prompt_too_long`)
   ([errors, Prompt is too long](https://code.claude.com/docs/en/errors#prompt-is-too-long)).
   It "doesn't run that recovery when a gateway rewrites the error to wording
   Claude Code doesn't recognize"
   ([model-config](https://code.claude.com/docs/en/model-config#correct-the-window-for-a-gateway-or-custom-model-id)).
   **[Inference]** Strata's wording ("request exceeds the context window…") is
   in none of those lists, so Claude Code most likely treats Strata's 400 as a
   plain request error: no smaller retry and no reactive compaction. The turn
   (or the `-p` run) ends. With `fit_max_tokens`, that request goes through
   instead.
3. **The compaction request itself fits.** **[Inference]** The summarization
   call carries the whole conversation at its fullest. If the conversation is
   just under the threshold, that call's prompt plus `max_tokens` is the
   request most likely to overflow. The clamp turns that overflow into a
   shorter summary instead of a failed compaction.
4. **A safety net for an auto-compact threshold that is slightly too high.**
   Token counts can drift: Strata counts with its own tokenizer, and
   **[Inference]** Claude Code sizes the conversation from the usage numbers
   the API reports. The clamp absorbs a few thousand tokens of that drift.

### Downsides

1. **Silent truncation of outputs.** The cap shrinks with no signal other than
   `stop_reason: "max_tokens"`. Near the end of the window the room can be a
   few hundred tokens, and long edits or file writes are cut off. Claude Code
   reports a cut-off reply as "Claude's response exceeded the … output token
   maximum"
   ([errors](https://code.claude.com/docs/en/errors#response-exceeded-the-output-token-maximum)).
   The docs suggest replying `continue`, which nobody does in an unattended
   `-p` run. **[Inference]**
2. **Tool calls cut in half.** A cut inside a tool call leaves a `tool_use`
   block with invalid partial JSON and `stop_reason: "max_tokens"` (source
   above). **[Inference]** Claude Code cannot run that call. The best case is
   a wasted turn. A partial `Write`/`Edit` payload is never applied, because
   the JSON doesn't parse.
3. **Thinking eats the small room.** When the room is clamped down near the
   1024 threshold, Strata's answer-room logic leaves the answer about 512
   tokens. Below 1024 a thinking model can spend the whole room thinking and
   give no answer (source above).
4. **It hides a bad auto-compact setup.** Without the option, a threshold that
   is too high fails loudly with a 400 (`tools/local-agent/README.md:56-59`
   uses exactly that string to diagnose it). With the option, the same mistake
   shows up as gradually shorter and truncated replies. **[Inference]**
5. **It does not help a prompt that is already too long.** `room < 1` is still
   a 400, so auto-compact must trigger in time anyway.
6. **It is a setting per model.** It lives in each `strata-<model>.json`, and
   Strata must restart before it applies. A model added later doesn't have it
   until someone sets it. **[Inference]** from where the key lives and when it
   is read.

**Net:** keep it on for Claude Code. The cost is a truncated reply near the end
of the window. That beats a hard 400 that Claude Code doesn't recover from.
Treat it as the safety net, and make auto-compact the actual mechanism.

## 2. Configuring Claude Code's auto-compact for Strata's smaller window

### What the docs say (each name checked in the docs)

| Name | What the docs say |
|---|---|
| `ANTHROPIC_BASE_URL` | Sends API requests through a proxy or gateway ([env-vars](https://code.claude.com/docs/en/env-vars)). |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | The auto-compact window in tokens, **from `100000` to `1000000`**, plain integers only, capped at the model's window. Takes precedence over `/autocompact`, `--autocompact` and `autoCompactWindow` ([env-vars](https://code.claude.com/docs/en/env-vars); [model-config](https://code.claude.com/docs/en/model-config#set-the-auto-compact-window)). |
| `autoCompactWindow` (setting), `--autocompact` (flag), `/autocompact` (command) | The same window, 100K–1M ([settings-reference](https://code.claude.com/docs/en/settings-reference#autocompactwindow); [commands](https://code.claude.com/docs/en/commands)). |
| `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | The percentage (1–100) **of the auto-compact window** at which compaction triggers. It can only lower the threshold: values above the default percentage are ignored. It "applies only in sessions that compact before the model's context limit" ([env-vars](https://code.claude.com/docs/en/env-vars)). |
| `CLAUDE_CODE_MAX_CONTEXT_TOKENS` | Overrides the window Claude Code assumes. For an unrecognized ID that does **not** start with `claude-` and has no `[1m]`, it "applies directly and proactive compaction continues at the declared window". For a recognized or bare `claude-` ID, it applies only together with `DISABLE_COMPACT` ([model-config](https://code.claude.com/docs/en/model-config#which-ids-claude_code_max_context_tokens-applies-to)). |
| `CLAUDE_CODE_DISABLE_1M_CONTEXT` | Holds models that run with 1M by default to a 200K window. With auto-compact on, they compact at the 200K boundary ([model-config](https://code.claude.com/docs/en/model-config#turn-off-1m-context)). |
| `CLAUDE_CODE_MAX_OUTPUT_TOKENS` | `max_tokens` for most requests. For an unresolved ID the default is 32000 and the cap 128000. "Increasing this value reduces the effective context window available before auto-compaction triggers" ([env-vars](https://code.claude.com/docs/en/env-vars)). |
| `DISABLE_AUTO_COMPACT` / `autoCompactEnabled` | Turn automatic compaction off (and idle compaction with it). `/compact` keeps working. The variable overrides the setting ([env-vars](https://code.claude.com/docs/en/env-vars); [settings-reference](https://code.claude.com/docs/en/settings-reference#autocompactenabled)). |
| `DISABLE_COMPACT` | Turns off all compaction, `/compact` included ([env-vars](https://code.claude.com/docs/en/env-vars)). |
| `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT` | For an unrecognized ID: compact only after the API rejects the request with a too-long error that Claude Code recognizes (v2.1.223+) ([env-vars](https://code.claude.com/docs/en/env-vars)). Wrong for Strata, because Strata's wording isn't recognized (see 1). |
| `/compact [instructions]`, `/context` | Compact manually with a focus. `/context` shows a grid of what fills the window ([commands](https://code.claude.com/docs/en/commands)). |

How Claude Code detects the window behind a proxy:

- **A Claude model name.** Claude Code gives each model it recognizes its
  Anthropic window and "can't detect a lower limit that the gateway or the
  server behind it enforces"
  ([model-config, gateway](https://code.claude.com/docs/en/model-config#context-window-behind-an-llm-gateway)).
- **Any other ID.** An unrecognized ID gets a startup warning and an assumed
  200K window. Sessions "compact at the context window Claude Code assumes for
  the ID" ([model-config](https://code.claude.com/docs/en/model-config#default-auto-compact-thresholds);
  changelog v2.1.223:
  "Changed auto-compact to keep sessions on unrecognized model IDs within the
  assumed context window").
- **Headless runs compact too.** The changelog for v2.1.300 fixes "resuming a
  headless or Agent SDK session losing the recent messages a compaction had
  kept" ([changelog](https://code.claude.com/docs/en/changelog)). This settles
  the open question in `tools/local-agent/README.md:46-48` in principle. The
  repo's hardware test still applies.
- **What fills the window after a compaction.** Startup content, the summary,
  up to 5 recently modified files (files over 5,000 tokens come back only as a
  path reference) and skill bodies (up to 25,000 tokens in total)
  ([context-window](https://code.claude.com/docs/en/context-window#what-survives-compaction)).
  **[Inference]** With a 64K window, the room after a compaction can be tight.

### Strata's real context size, according to the repo

- **Requirement for `local-agent`:** at least 64K (65536), and 128K if memory
  allows (`tools/local-agent/README.md:29-31`, `local-agent:46`).
- **Setup's recommendation:** 64K for a 14–20 GB card (`docs/strata-api.md:77`;
  `docs/setup.md:74`).
- **At run time:** Strata reports the actual value as `max_context` in
  `/health` (`docs/strata-api.md:26`). Read it there instead of assuming.

### The repo's current approach, and a risk in it

`local-agent` sets the following (`tools/local-agent/local-agent:349-357`;
plan: `/mnt/project-files/plaene/local-agent.md:28`):

- `ANTHROPIC_MODEL=claude-sonnet-4-5` (`local-agent:70`)
- `CLAUDE_CODE_DISABLE_1M_CONTEXT=1`
- `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE = 75 × max_context / 200000` (24 at 64K,
  `test.sh:119`)
- `CLAUDE_CODE_MAX_OUTPUT_TOKENS=8192`

**[Inference]** The docs say `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` applies "only in
sessions that compact before the model's context limit". Two more points from
the docs:

- Without a set window, Claude Code "compacts when the conversation reaches the
  model's context limit". The exceptions listed are 4.6 models without 1M,
  native-1M models under `CLAUDE_CODE_DISABLE_1M_CONTEXT`, native-1M models, and
  unrecognized IDs
  ([model-config](https://code.claude.com/docs/en/model-config#default-auto-compact-thresholds)).
- `claude-sonnet-4-5` is a recognized 200K model and is not in that list.

Read strictly, the override may therefore be **ignored** for this model, and
compaction would come at about 200K, far past Strata's 64K. The README's
hardware test (`README.md:41-59`) would show it as `request exceeds the context
window`. With `fit_max_tokens` on, it would show first as shrinking replies.
Two ways to harden this, both following the docs:

- **(A, keeps the Claude name):** also set `CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000`
  (the smallest allowed value). The session then explicitly compacts before
  the model's limit, and the percentage applies to a 100K window. 75 % of 64K
  becomes `PCT = 75 × 65536 / 100000 = 49`, so compaction comes at about 49K.
  For 128K, the 100K window alone is already about 76 %, so you can leave out
  `PCT`.
- **(B, declares the window):** use a non-`claude-` model ID such as `strata`
  plus `CLAUDE_CODE_MAX_CONTEXT_TOKENS=65536`. That is the documented path for
  "any other window". Caveats:
  - Strata's own docs say Claude Code "refuses names it doesn't" know
    ([S/docs/DETAILS.md#L980-L982](https://github.com/Niko1221/Strata/blob/477ac13/docs/DETAILS.md#L980-L982)).
    Current Claude Code docs say a model set through `ANTHROPIC_MODEL`,
    `--model` or `model` "isn't checked up front"
    ([model-config](https://code.claude.com/docs/en/model-config#setting-your-model)),
    and treat unknown IDs with a warning plus an assumed window. The two sources
    conflict. Test it before relying on it.
  - The docs don't say how far below the declared window compaction triggers.
    Here `fit_max_tokens` is the safety net.

### Recommended configuration (64K Strata, interactive use through the proxy)

Variant A in `~/.claude/settings.json`. The `env` block works for every session
([settings-reference, env](https://code.claude.com/docs/en/settings-reference#env)).
Model and endpoint go into user settings, not project settings:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://<NAS-IP>:8080",
    "ANTHROPIC_AUTH_TOKEN": "<key from Access>",
    "ANTHROPIC_MODEL": "claude-sonnet-4-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-sonnet-4-5",
    "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "100000",
    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "49",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "8192",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "API_TIMEOUT_MS": "900000"
  }
}
```

In Strata's `strata-<model>.json`: `"fit_max_tokens": true` (and
`"anthropic_thinking": "on_request"` per `tools/local-agent/README.md:34-35`).

Notes on the example:

- **The key.** The setup checklist uses `ANTHROPIC_API_KEY`, which goes out as
  `x-api-key` (`docs/setup.md:118`). The runner uses
  `ANTHROPIC_AUTH_TOKEN` (`local-agent:351`). The proxy accepts either header
  (`proxy.rs:161`).
- **Budget check.** About 49K for the compaction trigger, plus 8192 output,
  plus 8 slack, is about 57.4K, under 65536. That leaves about 8K of margin for
  the summary call and for token-count drift. **[Inference]**: the docs don't
  say whether Claude Code subtracts the output reserve before or after
  applying the percentage. Either way the result stays under 64K.
- **For 128K:** drop `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` (the 100K window ≈ 76 %
  of 131072). `CLAUDE_CODE_MAX_OUTPUT_TOKENS` can rise to 16384.
- **Keep `CLAUDE_CODE_MAX_OUTPUT_TOKENS` small.** A large value shrinks the
  window before compaction (env-vars). It also makes `fit_max_tokens` clamp
  more often, which means more silent truncation. 8192 fits Strata's own
  example of agents that cap replies at 8192
  ([#L4200-L4201](https://github.com/Niko1221/Strata/blob/477ac13/serve/server.py#L4200-L4201)).
- **Don't use `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT` or
  `DISABLE_AUTO_COMPACT`.** Both rely on Claude Code recognizing the API's
  too-long error, and Strata's wording isn't recognized.
- **Check it:** `/context` shows how full the window is. `/compact <focus>`
  compacts early before a big task. `/autocompact` can't go below 100K, so
  it is no substitute for the percentage override.

For `local-agent`: the same change in `worker_env` would mean
`CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000` and
`PCT = compact_share × probe_context / 100000`. That breaks the expectations in
`tools/local-agent/test.sh:119,131,425` (24 and 48). With 128K the value becomes
98, above any plausible default percentage, so you would leave it out.
Validate whichever variant you pick with the README's hardware run
(`tools/local-agent/README.md:50-59`).

## Hardware test (2026-10-19, Mac → NAS proxy → Strata)

Setup: Claude Code 2.1.218, `ANTHROPIC_MODEL=claude-sonnet-4-5`, Strata `max_context` 262144, `fit_max_tokens` on (probe with `max_tokens` 300000 → 200). Task: read and summarize the 37 files under `crates/nas-proxy/src` and `crates/admin-ui/src`, read-only, no subagents.

| Run | Settings | Result |
|---|---|---|
| Baseline | current runner (`PCT_OVERRIDE=98`, no window) | exit 0, 0 compactions, peak 179K |
| A | `AUTO_COMPACT_WINDOW=100000` | exit 0, 4 compactions at 80–89K, 23 min |
| B | window 100000 + `PCT_OVERRIDE=49` (64K-sized) | PCT honored (compactions at 61–70K), then exit 3: "Autocompact is thrashing" |
| C | window 150000 | exit 0, 1 compaction at 130K, peak 127K, 14 min |

No run hit "request exceeds the context window", nor were any tool calls truncated.

Measured: compaction fires at about window − `CLAUDE_CODE_MAX_OUTPUT_TOKENS` − ~13K.

Run #130 (write mode, before this test): context grew 36K → 253K over 181 turns with **zero** compactions. It ended on Strata's 400 (`fit_max_tokens` was off then). Without an explicit window, Claude Code 2.1.218 did not compact in `-p` mode against Strata at all. The cause is unverified.

Conclusions:
- Always set `CLAUDE_CODE_AUTO_COMPACT_WINDOW`. `fit_max_tokens` is a safety net only.
- Derive the window from `max_context`: window = clamp(75% × context + output + 13K, 100000, 180000). Use the PCT override only below that floor. This gives 256K → 180000 (compaction at ~159K), 128K → ~119K (~98K), and 64K → 100000 + PCT 49.
- 64K thrashes on read-heavy work: after a compaction the context is already 20–29K, and one file read adds up to 15K. Consider a minimum of 128K.
- 180000 rather than 200000, because 200000 (the model's own window) is untested.
