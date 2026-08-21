# ccusage (fork): prime-agent, fx, opencode2, pi + GitHub sync

Fork of [ccusage/ccusage](https://github.com/ccusage/ccusage) — analyze coding
agent CLI token usage and costs from local data — extended for a
multi-agent, multi-machine setup.

Everything upstream supports still works (`claude`, `codex`, `daily`,
`blocks`, `statusline`, ...). This fork adds:

## Extra agents

| Agent | Data source | Command |
| --- | --- | --- |
| **prime-agent** | `~/.prime/agent/sessions/*.jsonl` | `ccusage prime daily \| monthly \| session` |
| **fx (Vercel)** | `~/.fx/usage.jsonl` ledger + session index | `ccusage fx daily \| monthly \| session` |
| **opencode / opencode2** | `~/.local/share/opencode/opencode.db` (SQLite) and legacy JSON storage — both the stable and the new `@opencode-ai/cli` write here | `ccusage opencode daily \| weekly \| monthly \| session` |
| **pi** | `~/.pi/agent/sessions/` | `ccusage pi daily \| monthly \| session` |

Conductor.build itself stores no token usage in its own database; it drives
Claude Code / OpenCode inside its workspaces, so its sessions are already
counted through the claude and opencode sources above (verified: sessions
created by conductor appear in `ccusage opencode session`).

All agents also roll into the unified reports (`ccusage daily`, `session`,
`weekly`, `monthly`) with an Agent column.

### Model coverage

Every model string in the logs is reported as-is, including custom/proxy
models that have no public pricing (`ox-alpha-free`, `muse-spark-1.2-*`,
`deepseek-v4-*`, `zai/glm-5.2(-fast)`, ...). When a log embeds its own cost,
that display cost is used; free models correctly report `$0.00`. Unknown
pricing produces a warning instead of silently dropping tokens.

### Counting guarantees

- fx counts each generation exactly once from the authoritative root ledger;
  per-session event logs are used only for session/project attribution.
- pi-format sources (pi, prime) dedupe on message identity.
- OpenCode dedupes SQLite rows behind legacy JSON fallbacks.
- All sources were cross-checked against independent tallies of the raw logs.

## GitHub sync (`ccusage sync`)

Back up your stats to a private GitHub repo and coalesce usage across all
your machines:

```sh
gh repo create ccusage-sync --private   # once
ccusage sync --repo <you>/ccusage-sync  # parse → push → pull → coalesced report
```

- Each machine publishes only its own file (`data/<machine>.json`), so merges
  are conflict-free by construction and re-running is a no-op when nothing
  changed.
- The report combines every machine: date × agent (`pi @macbook`) with token
  and cost totals. `--json` gives the full merged structure.
- Options: `--machine <name>` (default hostname), `--no-push` (merge without
  publishing), env `CCUSAGE_SYNC_REPO`.

## Readability

- Numbers never truncate mid-digit: cells that do not fit render as
  magnitudes (`68,442,263` → `68.44M`).
- Unified reports drop redundant `[store]` prefixes from model labels since
  the Agent column already says which tool they came from.

## Install alias

The npm package exposes both binaries:

```sh
npm i -g @<your-scope>/ccusage   # or link this checkout
ccusage2 daily                    # identical to ccusage
```

## Building from source

```sh
cd rust
CCUSAGE_PRICING_JSON_PATH=/path/to/model_prices_and_context_window.json cargo build --release
# snapshot pinned by flake.lock:
curl -sL -o /tmp/litellm.json \
  https://raw.githubusercontent.com/BerriAI/litellm/2dcd45386045b44f5a61952094a3f14c9cbf504e/model_prices_and_context_window.json
```

---

Below: original upstream README content.
