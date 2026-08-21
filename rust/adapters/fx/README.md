# fx Source

Data source (per data directory):

```text
${FX_HOME:-~/.fx}/usage.jsonl          # authoritative generation ledger
${FX_HOME:-~/.fx}/sessions/*/events.jsonl   # session attribution only
```

The root `usage.jsonl` ledger holds every generation fact exactly once, so it
is the single source of token counts. Per-session `events.jsonl` files carry
cumulative snapshots without stable generation ids and are deliberately not
counted; they only contribute session identity (`session_started` payloads:
id, start time, workspace root).

Session attribution: each ledger generation is assigned to the most recently
started session at or before its timestamp. fx sessions can run in parallel,
so "latest start wins" keeps assignment deterministic while every generation
still counts exactly once. The session's `workspace_root` becomes the project
label. Generations older than every session land under `(no session)` with
the data directory name as project.

Token mapping:

- `inputTokens` <- `fact.input_tokens`
- `outputTokens` <- `fact.output_tokens`
- `cacheReadInputTokens` <- `fact.cache_read_tokens`
- `cacheCreationInputTokens` <- `fact.cache_write_tokens`
- `reasoning_tokens` are already included in `output_tokens` by the writer.

Model labels render as `[fx] <model>`. Embedded `fact.total_cost` is used as
the display cost when present (many fx generations bill at zero).

Commands:

```sh
ccusage fx daily
ccusage fx monthly
ccusage fx session
ccusage fx daily --json
ccusage fx daily --fx-path /path/to/fx-home
```
