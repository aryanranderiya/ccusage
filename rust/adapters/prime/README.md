# Prime Agent Source

Data source:

```text
${PRIME_AGENT_DIR:-~/.prime/agent/sessions/}
```

Prime-agent writes pi-format v3 session JSONL files, so loading, dedupe, cost
semantics, and summaries delegate to the pi adapter's named-store surface
under the `prime` label. Model labels render as `[prime] <model>`.

Token mapping:

- `inputTokens` <- `message.usage.input`
- `outputTokens` <- `message.usage.output`
- `cacheReadInputTokens` <- `message.usage.cacheRead`
- `cacheCreationInputTokens` <- `message.usage.cacheWrite`

Messages may include a pre-calculated `cost.total`; auto mode prefers it and
otherwise prices from the raw model name.

Commands:

```sh
ccusage prime daily
ccusage prime monthly
ccusage prime session
ccusage prime daily --json
ccusage prime daily --prime-path /path/to/sessions
```
