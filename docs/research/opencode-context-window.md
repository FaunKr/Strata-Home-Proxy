# How opencode learns a model's context window, and what that means for the proxy

Research note, 2026-10-25. Sources: opencode at tag `v1.19.3` (the version
`tools/code-bench/Dockerfile` pins), commit `e3c4eb2`; Strata at `41cf05a`
(2026-10-25); this repo at `origin/main` `3eda4d8`.
`OC` = `https://github.com/sst/opencode/blob/v1.19.3/packages`.
**[Inference]** marks conclusions no source states directly.

## Short answer

opencode takes a model's limits from exactly two places: the `limit` block of
that model in `opencode.json`, or the models.dev catalog entry with the same
provider id and model id. It never reads `/v1/models` (or `/health`, `/props`)
of an `@ai-sdk/openai-compatible` provider. For a custom provider like
`strata`, models.dev has no entry, so without `limit` in the config the
context is **0** and the output cap is opencode's default **32000**.

Context 0 does not mean "unlimited" in a harmful way, it means "unknown": opencode
turns off proactive compaction and only compacts after the server rejects a
request as too long. With Strata that reactive path does fire (Strata's 400
wording matches opencode's patterns), but it comes late and can fail.

So: **the user must set `limit.context` (and ideally `limit.output`) in their
own opencode config**, and the code benchmark's runner should write them too.
Nothing the proxy puts into `/v1/models` (Strata's `meta.n_ctx`, the public
name of #373) changes what opencode assumes.

## 1. Where opencode's limits come from

- **Config merge.** For every model under `provider.<id>.models` opencode builds
  `limit: { context: model.limit?.context ?? existingModel?.limit?.context ?? 0,
  input: …, output: model.limit?.output ?? existingModel?.limit?.output ?? 0 }`
  ([OC/opencode/src/provider/provider.ts#L1609-L1613](https://github.com/sst/opencode/blob/v1.19.3/packages/opencode/src/provider/provider.ts#L1609)).
  `existingModel` is the models.dev entry under the same provider id and model
  id (`parsed.models[model.id ?? modelID]`, #L1545). A provider id that is not
  in models.dev (`strata`) has no entries, so the fallback is `0`.
- **models.dev.** Loaded from the disk cache, else a bundled snapshot, else
  fetched; `OPENCODE_DISABLE_MODELS_FETCH` only stops the fetch
  ([OC/core/src/models-dev.ts#L216-L230](https://github.com/sst/opencode/blob/v1.19.3/packages/core/src/models-dev.ts#L216)).
  It is keyed by provider, so it never helps a provider named `strata`.
  **[Inference]** Naming the provider `openai` and the model e.g. `gpt-4o` to
  borrow models.dev's limits would give wrong numbers; not a route.
- **No discovery from the server.** No code path fetches `/v1/models` for an
  openai-compatible provider. Only three bundled plugins read a model list
  with context sizes (GitHub Copilot, Azure, Modal's `context_length`,
  `OC/opencode/src/plugin/modal/models.ts#L86`); none applies here.
- **Docs.** "`limit.context`: Maximum input tokens the model accepts.
  `limit.output`: Maximum tokens the model can generate. The `limit` fields allow
  OpenCode to understand how much context you have left. Standard providers pull
  these from models.dev automatically"
  ([OC/web/src/content/docs/providers.mdx#L2666-L2669](https://github.com/sst/opencode/blob/v1.19.3/packages/web/src/content/docs/providers.mdx#L2666)).

## 2. What the limits drive

- **Output cap per request.** `maxOutputTokens = min(limit.output, 32000) || 32000`
  ([OC/opencode/src/provider/transform.ts#L1481-L1483](https://github.com/sst/opencode/blob/v1.19.3/packages/opencode/src/provider/transform.ts#L1481),
  `OUTPUT_TOKEN_MAX = 32_000` at #L18). The env var
  `OPENCODE_EXPERIMENTAL_OUTPUT_TOKEN_MAX` replaces the 32000
  (`OC/opencode/src/effect/runtime-flags.ts#L52`). So without `limit.output`
  every request asks for `max_tokens` 32000; `limit.output` above 32000 is
  capped to 32000.
- **Proactive compaction.** After each turn, opencode compacts when the reported
  usage (`tokens.total`, or input+output+cache) reaches
  `usable = context − maxOutputTokens` (or `limit.input − min(20000, maxOutputTokens)`
  when `limit.input` is set; `compaction.reserved` replaces that 20000 buffer).
  `isOverflow` returns false when `limit.context === 0`
  ([OC/opencode/src/session/overflow.ts](https://github.com/sst/opencode/blob/v1.19.3/packages/opencode/src/session/overflow.ts)).
  `compaction.auto: false` turns it off.
- **Reactive compaction.** A provider error whose message matches one of
  opencode's overflow patterns (or status 413, or `code: context_length_exceeded`)
  becomes a `ContextOverflowError`
  ([OC/opencode/src/provider/error.ts#L175](https://github.com/sst/opencode/blob/v1.19.3/packages/opencode/src/provider/error.ts#L175)),
  and the processor sets `needsCompaction` instead of failing the turn
  (`OC/opencode/src/session/processor.ts#L621-L630`). One of the patterns is
  `/exceeds the context window/i`
  ([OC/llm/src/provider-error.ts#L8](https://github.com/sst/opencode/blob/v1.19.3/packages/llm/src/provider-error.ts#L8)).
  If the compaction request itself overflows, the session stops with "Session
  too large to compact - context exceeds model limit…"
  (`OC/opencode/src/session/compaction.ts#L451-L455`).

## 3. How this meets Strata and the proxy

- **Strata's 400 is recognized.** Both of Strata's refusals start with
  "request exceeds the context window: …" (`serve/server.py#L4895`, `#L4902`),
  which matches opencode's pattern, so opencode compacts instead of erroring.
  The proxy passes the 400 through unchanged.
- **`max_tokens` 32000 is harmless now.** Strata clamps an oversized
  `max_tokens` to the room left by default (`fit_max_tokens` defaults to true,
  `serve/server.py#L3824-L3827`; `docs/strata-api.md` notes it since f31b6efd).
  The only 400 left is a prompt that alone does not fit.
- **But with `context` 0 the only compaction is the late one.** **[Inference]**
  opencode then grows the conversation until the prompt alone exceeds Strata's
  context, compacts only then, and has to send that full, too-long history to
  be summarized. That is the case that ends in "too large to compact". Before
  that, replies near the end of the window get silently cut short by the clamp
  (see `fit-max-tokens-and-autocompact.md`, downside 1). With `limit.context`
  set, opencode compacts at `context − output` while there is still room.
- **`/v1/models` and the public name (#373) do not matter to opencode.** The
  model id in `opencode.json` is just the name opencode sends; Strata serves
  the loaded model whatever the name (`docs/strata-api.md`, "Model names in
  requests and answers"). A hiding key's public name like `strata` works as a
  config model id. Strata's `meta.n_ctx` in the list is never read.
- **One config entry, changing models.** **[Inference]** `limit.context` is a
  fixed number in the user's config, while the proxy can switch the active
  model, and its context with it. Behind a public name the user cannot tell
  which model answers. A too-high value brings back the late-compaction
  failure; a too-low value only compacts early. The safe value is the
  smallest context among the models the user reaches through that entry.

## 4. The code benchmark before #378

`tools/code-bench/runner.py` `config()` wrote the proxy as provider `strata`
with one model, `{"name": model, "temperature": False}`, and **no `limit`**.
The runner also sets `OPENCODE_DISABLE_MODELS_FETCH=1`. So in every benchmark
task opencode had context 0, never compacted proactively, and sent
`max_tokens` 32000. `tools/code-bench/README.md` assumed opencode compacts
on long repo tasks ("so it finds its place again after opencode compacts"):
that only happened via Strata's 400, and could end the task with "too large to
compact" instead. #378 sends the model's context with the run and writes the
`limit` described in section 5.

## 5. What was changed (#377, #378)

- **The user's own opencode:** the README's *opencode* part shows the
  provider with `limit.context` (the model's context, or the smallest one
  across models behind one public name) and `limit.output` (a quarter of the
  context, at most 32000).
- **Code benchmark:** the proxy sends the model's start context from the PC
  service's list of models in `POST /runs` (`context`, or `null`). The runner
  writes `"limit": {"context": context, "output": min(32000, context // 4)}`,
  and no `limit` without a context.
- **Proxy `/v1/models`:** nothing changed; opencode does not read it.
