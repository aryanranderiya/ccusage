use std::{
    collections::BTreeSet,
    io::{BufWriter, IsTerminal, Write},
};

use serde::{
    Serialize,
    ser::{SerializeMap, Serializer},
};
use serde_json::{Value, json};

use crate::{
    Align, Color, ModelBreakdown, Result, SimpleTable, UsageSummary,
    cli::{AgentReportKind, SharedArgs, SortOrder},
    cli_error, color, format_currency, format_models_multiline, format_number, json_float,
    output::strip_cost_json,
    print_box_title, short_model_name, should_use_compact_layout,
};

use super::types::AllRow;

#[cfg(test)]
pub(super) fn report_json(rows: &[AllRow], kind: AgentReportKind) -> Value {
    report_json_with_agents(rows, kind, false)
}

pub(super) fn report_json_with_agents(
    rows: &[AllRow],
    kind: AgentReportKind,
    include_agents: bool,
) -> Value {
    json!({
        rows_key(kind): rows.iter().map(|row| row_json(row, include_agents)).collect::<Vec<_>>(),
        "totals": totals_json(rows),
    })
}

pub(super) fn sections_report_json(
    sections: &[(AgentReportKind, Vec<AllRow>)],
    command_kind: AgentReportKind,
    include_agents: bool,
) -> OrderedJsonMap {
    let mut fields = Vec::with_capacity(sections.len() + 1);
    for (kind, rows) in sections {
        fields.push((
            rows_key(*kind),
            Value::Array(
                rows.iter()
                    .map(|row| row_json(row, include_agents))
                    .collect(),
            ),
        ));
    }
    let command_rows = sections
        .iter()
        .find_map(|(kind, rows)| (*kind == command_kind).then_some(rows.as_slice()))
        .unwrap_or(&[]);
    fields.push(("totals", totals_json(command_rows)));
    OrderedJsonMap { fields }
}

pub(super) fn print_sections_report_json(
    sections: &[(AgentReportKind, Vec<AllRow>)],
    command_kind: AgentReportKind,
    include_agents: bool,
    jq: Option<&str>,
    no_cost: bool,
) -> Result<()> {
    let mut report = sections_report_json(sections, command_kind, include_agents);
    if no_cost {
        report.strip_costs();
    }
    if let Some(filter) = jq {
        let mut child = std::process::Command::new("jq")
            .arg(filter)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .spawn()
            .map_err(|error| cli_error(format!("failed to run jq: {error}")))?;
        if let Some(stdin) = child.stdin.take() {
            let mut stdin = BufWriter::new(stdin);
            serde_json::to_writer(&mut stdin, &report)?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(cli_error("jq failed"));
        }
    } else {
        let stdout = std::io::stdout();
        let mut stdout = BufWriter::new(stdout.lock());
        serde_json::to_writer_pretty(&mut stdout, &report)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

pub(super) struct OrderedJsonMap {
    fields: Vec<(&'static str, Value)>,
}

impl OrderedJsonMap {
    #[cfg(test)]
    pub(super) fn get(&self, key: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find_map(|(field, value)| (*field == key).then_some(value))
    }

    fn strip_costs(&mut self) {
        for (_, value) in &mut self.fields {
            strip_cost_json(value);
        }
    }
}

impl Serialize for OrderedJsonMap {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (key, value) in &self.fields {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn row_json(row: &AllRow, include_agents: bool) -> Value {
    let mut value = agent_json(row);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("period".to_string(), json!(row.period));
    }
    if let (Some(obj), Some(agents)) = (value.as_object_mut(), row.metadata_agents.as_ref()) {
        obj.insert(
            "metadata".to_string(),
            row.metadata
                .clone()
                .unwrap_or_else(|| json!({ "agents": agents })),
        );
    } else if let (Some(obj), Some(metadata)) = (value.as_object_mut(), row.metadata.as_ref()) {
        obj.insert("metadata".to_string(), metadata.clone());
    }
    if include_agents
        && let (Some(obj), Some(agent_breakdowns)) =
            (value.as_object_mut(), row.agent_breakdowns.as_ref())
    {
        obj.insert(
            "agents".to_string(),
            Value::Array(agent_breakdowns.iter().map(agent_json).collect()),
        );
    }
    value
}

fn agent_json(row: &AllRow) -> Value {
    json!({
        "agent": row.agent,
        "modelsUsed": row.models_used,
        "inputTokens": row.input_tokens,
        "outputTokens": row.output_tokens,
        "cacheCreationTokens": row.cache_creation_tokens,
        "cacheReadTokens": row.cache_read_tokens,
        "totalTokens": row.total_tokens,
        "totalCost": json_float(row.total_cost),
        "modelBreakdowns": row.model_breakdowns,
    })
}

fn totals_json(rows: &[AllRow]) -> Value {
    json!({
        "inputTokens": rows.iter().map(|row| row.input_tokens).sum::<u64>(),
        "outputTokens": rows.iter().map(|row| row.output_tokens).sum::<u64>(),
        "cacheCreationTokens": rows.iter().map(|row| row.cache_creation_tokens).sum::<u64>(),
        "cacheReadTokens": rows.iter().map(|row| row.cache_read_tokens).sum::<u64>(),
        "totalTokens": rows.iter().map(|row| row.total_tokens).sum::<u64>(),
        "totalCost": json_float(rows.iter().map(|row| row.total_cost).sum::<f64>()),
    })
}

fn rows_key(kind: AgentReportKind) -> &'static str {
    match kind {
        AgentReportKind::Daily => "daily",
        AgentReportKind::Weekly => "weekly",
        AgentReportKind::Monthly => "monthly",
        AgentReportKind::Session => "session",
    }
}

pub(super) fn print_table(
    rows: &[AllRow],
    kind: AgentReportKind,
    shared: &SharedArgs,
    detected_agents: &[&'static str],
) -> Result<()> {
    print_box_title(&all_report_title(kind, rows, detected_agents), shared);
    if rows.is_empty() {
        eprintln!("No usage data found.");
        return Ok(());
    }
    let terminal_width = crate::terminal_width();
    let is_tty = std::io::stdout().is_terminal();
    let compact = should_use_compact_layout(
        shared,
        is_tty,
        terminal_width,
        crate::USAGE_COMPACT_WIDTH_THRESHOLD,
    );
    let (headers, aligns) = all_table_columns(kind, compact, shared.no_cost);
    let mut table = SimpleTable::new(headers, aligns, crate::terminal_style(shared))
        .with_terminal_width(terminal_width)
        .with_date_compaction(true);

    let mut current_period: Option<String> = None;
    for row in rows {
        let values = all_table_row(row, compact, false, shared.no_cost);
        if current_period.as_deref() != Some(row.period.as_str()) {
            // A separator above each day block gives the report its rhythm;
            // rows inside a day flow without lines.
            if current_period.is_some() {
                table.separator();
            }
            current_period = Some(row.period.clone());
        }
        let new_period = true;
        let mut styled = values;
        if !compact {
            if new_period && !styled[0].is_empty() {
                styled[0] = color(shared, styled[0].clone(), Color::Bold);
            }
            if styled[1] == "All" {
                styled[1] = color(shared, styled[1].clone(), Color::Bold);
            }
            if row.total_cost == 0.0 {
                let index = styled.len() - 1;
                styled[index] = color(shared, styled[index].clone(), Color::Grey);
            }
        }
        table.push(styled);
        if let Some(agent_breakdowns) = row.agent_breakdowns.as_ref() {
            for (index, breakdown) in agent_breakdowns.iter().enumerate() {
                // Without a line between them, adjacent single-line agent rows
                // read as one merged block; separate each agent after the first.
                if index > 0 {
                    table.separator();
                }
                table.push(all_table_row(breakdown, compact, true, shared.no_cost));
                if shared.breakdown && !breakdown.model_breakdowns.is_empty() {
                    push_model_breakdown_rows(
                        &mut table,
                        &breakdown.model_breakdowns,
                        compact,
                        shared,
                    );
                }
            }
        } else if shared.breakdown && !row.model_breakdowns.is_empty() {
            push_model_breakdown_rows(&mut table, &row.model_breakdowns, compact, shared);
        }
    }
    table.separator();
    let totals = totals_json(rows);
    let table_total_tokens = rows.iter().map(table_total_tokens).sum::<u64>();
    if compact {
        let mut total_row = vec![
            color(shared, "Total", Color::Yellow),
            String::new(),
            String::new(),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("inputTokens"))),
                Color::Yellow,
            ),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("outputTokens"))),
                Color::Yellow,
            ),
            color(
                shared,
                format_currency(
                    totals
                        .get("totalCost")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                ),
                Color::Yellow,
            ),
        ];
        if shared.no_cost {
            total_row.pop();
        }
        table.push(total_row);
    } else {
        let mut total_row = vec![
            color(shared, "Total", Color::Yellow),
            String::new(),
            String::new(),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("inputTokens"))),
                Color::Yellow,
            ),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("outputTokens"))),
                Color::Yellow,
            ),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("cacheCreationTokens"))),
                Color::Yellow,
            ),
            color(
                shared,
                format_number(crate::json_value_u64(totals.get("cacheReadTokens"))),
                Color::Yellow,
            ),
            color(shared, format_number(table_total_tokens), Color::Yellow),
            color(
                shared,
                format_currency(
                    totals
                        .get("totalCost")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                ),
                Color::Yellow,
            ),
        ];
        if shared.no_cost {
            total_row.pop();
        }
        table.push(total_row);
    }
    table.print()?;
    crate::print_missing_pricing_warnings(&all_rows_as_usage_summaries(rows), shared.offline);
    if compact {
        eprintln!("\nRunning in Compact Mode");
        eprintln!("Expand terminal width to see cache metrics and total tokens");
    }
    Ok(())
}

fn all_rows_as_usage_summaries(rows: &[AllRow]) -> Vec<UsageSummary> {
    rows.iter()
        .map(|row| UsageSummary {
            date: None,
            month: None,
            week: None,
            session_id: None,
            project_path: None,
            last_activity: None,
            first_activity: None,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_creation_tokens: row.cache_creation_tokens,
            cache_read_tokens: row.cache_read_tokens,
            extra_total_tokens: row.total_tokens.saturating_sub(table_total_tokens(row)),
            total_cost: row.total_cost,
            credits: None,
            message_count: None,
            models_used: row.models_used.clone(),
            model_breakdowns: row.model_breakdowns.clone(),
            project: None,
            versions: None,
        })
        .collect()
}

pub(super) fn all_report_title(
    kind: AgentReportKind,
    rows: &[AllRow],
    detected_agents: &[&'static str],
) -> String {
    format!(
        "Coding (Agent) CLI Usage Report - {}\nDetected: {}",
        match kind {
            AgentReportKind::Daily => "Daily",
            AgentReportKind::Weekly => "Weekly",
            AgentReportKind::Monthly => "Monthly",
            AgentReportKind::Session => "Session",
        },
        detected_agent_labels(rows, detected_agents)
    )
}

fn detected_agent_labels(rows: &[AllRow], detected_agents: &[&'static str]) -> String {
    let mut agents = BTreeSet::new();
    if detected_agents.is_empty() {
        for row in rows {
            if let Some(metadata_agents) = row.metadata_agents.as_ref() {
                agents.extend(metadata_agents.iter().copied());
            } else if row.agent != "all" {
                agents.insert(row.agent);
            }
            if let Some(breakdowns) = row.agent_breakdowns.as_ref() {
                agents.extend(breakdowns.iter().map(|breakdown| breakdown.agent));
            }
        }
    } else {
        agents.extend(detected_agents.iter().copied());
    }
    if agents.is_empty() {
        return "None".to_string();
    }
    agents
        .into_iter()
        .map(agent_label)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn all_table_row(
    row: &AllRow,
    compact: bool,
    breakdown: bool,
    no_cost: bool,
) -> Vec<String> {
    let period = if breakdown {
        String::new()
    } else {
        row.period.clone()
    };
    let agent = if breakdown {
        format!("- {}", agent_label(row.agent))
    } else if row.agent_breakdowns.is_some() {
        "All".to_string()
    } else {
        agent_label(row.agent).to_string()
    };
    let models = if row.agent_breakdowns.is_some() {
        String::new()
    } else {
        unified_models_multiline(&row.models_used)
    };

    if compact {
        let mut values = vec![
            period,
            agent,
            models,
            format_number(row.input_tokens),
            format_number(row.output_tokens),
            format_currency(row.total_cost),
        ];
        if no_cost {
            values.pop();
        }
        return values;
    }

    let mut values = vec![
        period,
        agent,
        models,
        format_number(row.input_tokens),
        format_number(row.output_tokens),
        format_number(row.cache_creation_tokens),
        format_number(row.cache_read_tokens),
        format_number(table_total_tokens(row)),
        format_currency(row.total_cost),
    ];
    if no_cost {
        values.pop();
    }
    values
}

/// Unified reports carry an Agent column, so per-row model labels drop their
/// `[store] ` prefixes ("[pi] deepseek-v4-flash" -> "deepseek-v4-flash") and
/// spend that width on the model name instead.
fn strip_store_prefix(model: &str) -> &str {
    let Some(rest) = model.strip_prefix('[') else {
        return model;
    };
    match rest.split_once(']') {
        Some((_, after)) => after.strip_prefix(' ').unwrap_or(after),
        None => model,
    }
}

fn unified_models_multiline(models: &[String]) -> String {
    let stripped = models
        .iter()
        .map(|model| strip_store_prefix(model).to_string())
        .collect::<Vec<_>>();
    format_models_multiline(&stripped)
}

fn table_total_tokens(row: &AllRow) -> u64 {
    row.input_tokens
        .saturating_add(row.output_tokens)
        .saturating_add(row.cache_creation_tokens)
        .saturating_add(row.cache_read_tokens)
}

fn push_model_breakdown_rows(
    table: &mut SimpleTable,
    breakdowns: &[ModelBreakdown],
    compact: bool,
    shared: &SharedArgs,
) {
    for b in breakdowns {
        let total =
            b.input_tokens + b.output_tokens + b.cache_creation_tokens + b.cache_read_tokens;
        let model = color(
            shared,
            format!("- {}", short_model_name(strip_store_prefix(&b.model_name))),
            Color::Grey,
        );
        if compact {
            let mut row = vec![
                String::new(),
                String::new(),
                model,
                color(shared, format_number(b.input_tokens), Color::Grey),
                color(shared, format_number(b.output_tokens), Color::Grey),
                color(shared, format_currency(b.cost), Color::Grey),
            ];
            if shared.no_cost {
                row.pop();
            }
            table.push(row);
        } else {
            let mut row = vec![
                String::new(),
                String::new(),
                model,
                color(shared, format_number(b.input_tokens), Color::Grey),
                color(shared, format_number(b.output_tokens), Color::Grey),
                color(shared, format_number(b.cache_creation_tokens), Color::Grey),
                color(shared, format_number(b.cache_read_tokens), Color::Grey),
                color(shared, format_number(total), Color::Grey),
                color(shared, format_currency(b.cost), Color::Grey),
            ];
            if shared.no_cost {
                row.pop();
            }
            table.push(row);
        }
    }
}

pub(super) fn all_table_columns(
    kind: AgentReportKind,
    compact: bool,
    no_cost: bool,
) -> (Vec<&'static str>, Vec<Align>) {
    let (mut headers, mut aligns) = if compact {
        (
            vec![
                first_column(kind),
                "Agent",
                "Models",
                "Input",
                "Output",
                "Cost (USD)",
            ],
            vec![
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        )
    } else {
        (
            vec![
                first_column(kind),
                "Agent",
                "Models",
                "Input",
                "Output",
                "Cache Create",
                "Cache Read",
                "Total Tokens",
                "Cost (USD)",
            ],
            vec![
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        )
    };
    if no_cost {
        headers.pop();
        aligns.pop();
    }
    (headers, aligns)
}

pub(super) fn sort_rows(rows: &mut [AllRow], order: &SortOrder) {
    rows.sort_by(|a, b| match a.period.cmp(&b.period) {
        std::cmp::Ordering::Equal => a.agent.cmp(b.agent),
        order => order,
    });
    if *order == SortOrder::Desc {
        rows.reverse();
    }
}

fn first_column(kind: AgentReportKind) -> &'static str {
    match kind {
        AgentReportKind::Daily => "Date",
        AgentReportKind::Weekly => "Week",
        AgentReportKind::Monthly => "Month",
        AgentReportKind::Session => "Session",
    }
}

fn agent_label(agent: &str) -> &str {
    match agent {
        "all" => "All",
        "claude" => "Claude",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "amp" => "Amp",
        "droid" => "Droid",
        "codebuff" => "Codebuff",
        "hermes" => "Hermes",
        "pi" => "pi-agent",
        "goose" => "Goose",
        "openclaw" => "OpenClaw",
        "kilo" => "Kilo",
        "copilot" => "GitHub Copilot CLI",
        "gemini" => "Gemini CLI",
        "kimi" => "Kimi",
        "qwen" => "Qwen",
        "grok" => "Grok",
        _ => agent,
    }
}

/// Data for one rendered section of the summary view; plain values only, so
/// layout math never touches ANSI escapes.
pub(super) struct SummaryDay {
    pub label: String,
    pub total_cost: f64,
    pub agents: Vec<SummaryAgent>,
}

pub(super) struct SummaryAgent {
    pub name: String,
    pub total_cost: f64,
    pub models: Vec<SummaryModel>,
}

pub(super) struct SummaryModel {
    pub name: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
}

/// Renders the summary body into plain-text lines. All columns derive from
/// one shared grid: money right-aligns to a single rail, tokens to their own
/// rails, and nothing else may push them around. Pure function -> testable.
fn summary_body_lines(days: &[SummaryDay], width: usize) -> Vec<String> {
    const MAX_WIDTH: usize = 104;
    let width = width.min(MAX_WIDTH).max(72);

    let all_models: Vec<&SummaryModel> = days
        .iter()
        .flat_map(|day| day.agents.iter())
        .flat_map(|agent| agent.models.iter())
        .collect();

    let name_width = all_models
        .iter()
        .map(|model| model.name.len())
        .max()
        .unwrap_or(6)
        .clamp(6, 28);
    let in_width = all_models
        .iter()
        .map(|model| format!("↑{}", short_tokens(model.input)).len())
        .max()
        .unwrap_or(4)
        .clamp(4, 9);
    let out_width = all_models
        .iter()
        .map(|model| format!("↓{}", short_tokens(model.output)).len())
        .max()
        .unwrap_or(4)
        .clamp(4, 9);

    // One money rail: widest dollar figure anywhere sets the column.
    let money_width = all_models
        .iter()
        .map(|model| format_currency(model.cost).len())
        .chain(
            days.iter()
                .flat_map(|day| day.agents.iter())
                .map(|agent| format_currency(agent.total_cost).len()),
        )
        .chain(days.iter().map(|day| format_currency(day.total_cost).len()))
        .chain(std::iter::once(
            format_currency(days.iter().map(|day| day.total_cost).sum()).len(),
        ))
        .max()
        .unwrap_or(8)
        .clamp(8, 13);

    // Model line: 6(indent) + name + 1 + in + 1 + out + 1 + CACHE_FIELD + 1
    // + money must equal `width` exactly, which sizes the cache field so the
    // money rail lands on the same column as every other row.
    // Gaps: indent(6) + name + sp + in + sp + out + sp + cache + sp + money.
    let cache_field =
        width.saturating_sub(10 + name_width + in_width + out_width + money_width);

    let mut lines = Vec::new();
    for (index, day) in days.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        // Day header: date left, thin rule, cost on the rail.
        let label = format!(" {}", day.label);
        let fill = width.saturating_sub(label.len() + 1 + money_width).max(3);
        lines.push(format!(
            "{label}{} {:>mw$}",
            "─".repeat(fill),
            format_currency(day.total_cost),
            mw = money_width
        ));

        for (agent_index, agent) in day.agents.iter().enumerate() {
            if agent_index > 0 {
                lines.push(String::new());
            }
            let leader = width
                .saturating_sub(3 + agent.name.len() + 1 + money_width)
                .max(3);
            lines.push(format!(
                "   {}{} {:>mw$}",
                agent.name,
                "·".repeat(leader),
                format_currency(agent.total_cost),
                mw = money_width
            ));
            for model in &agent.models {
                let cache_text = cache_segment(model.cache_read, model.cache_write);
                let cache_pad = cache_field.saturating_sub(cache_text.len());
                lines.push(format!(
                    "      {:<nw$} {:>iw$} {:>ow$} {}{} {:>mw$}",
                    model.name,
                    format!("↑{}", short_tokens(model.input)),
                    format!("↓{}", short_tokens(model.output)),
                    " ".repeat(cache_pad),
                    cache_text,
                    format_currency(model.cost),
                    nw = name_width,
                    iw = in_width,
                    ow = out_width,
                    mw = money_width,
                ));
            }
        }
    }

    // Grand total on the same rail.
    let grand = format_currency(days.iter().map(|day| day.total_cost).sum());
    let meta = format!(
        "{} days · {} tokens",
        days.len(),
        short_tokens(all_models.iter().map(|m| m.input + m.output + m.cache_read + m.cache_write).sum())
    );
    let used = 2 + "TOTAL".len() + 3 + meta.len() + money_width;
    let fill = width.saturating_sub(used).max(3);
    lines.push(String::new());
    lines.push(format!(
        "  TOTAL{} {} {:>mw$}",
        format!("  {meta}"),
        "─".repeat(fill),
        grand,
        mw = money_width
    ));
    lines
}

/// Collects the loaded report into summary sections and prints the TTY view.
pub(super) fn print_summary_view(
    rows: &[AllRow],
    kind: AgentReportKind,
    shared: &SharedArgs,
    detected_agents: &[&'static str],
) -> Result<()> {
    println!();
    print_box_title(&all_report_title(kind, rows, detected_agents), shared);
    if rows.is_empty() {
        eprintln!("No usage data found.");
        return Ok(());
    }

    let days: Vec<SummaryDay> = rows
        .iter()
        .map(|row| SummaryDay {
            label: humanize_period(&row.period),
            total_cost: row.total_cost,
            agents: agent_breakdowns(row)
                .iter()
                .filter(|agent| agent.total_cost != 0.0 || !agent.models_used.is_empty())
                .map(|agent| SummaryAgent {
                    name: strip_store_prefix(agent.agent).to_string(),
                    total_cost: agent.total_cost,
                    models: visible_models(agent)
                        .map(|model| SummaryModel {
                            name: short_model_name(strip_store_prefix(&model.model_name)),
                            input: model.input_tokens,
                            output: model.output_tokens,
                            cache_read: model.cache_read_tokens,
                            cache_write: model.cache_creation_tokens,
                            cost: model.cost,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    let width = (crate::terminal_width().max(72) as usize).min(104);
    let dim = |value: String| color(shared, value, Color::Grey);
    let bold = |value: String| color(shared, value, Color::Bold);

    for line in summary_body_lines(&days, width) {
        // Style the two structural elements; the body keeps its own subtle
        // markers by wrapping known prefixes.
        if line.starts_with(' ') && line.contains('─') && !line.contains("↑") && !line.contains("TOTAL") {
            println!("{}", dim(line));
        } else if line.trim_start().starts_with("TOTAL") || line.contains("──") && !line.contains("↑") {
            println!("{}", bold(line));
        } else {
            println!("{line}");
        }
    }
    Ok(())
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `2026-08-22` -> `Aug 22`; weekly (`2026-W33`) and monthly (`2026-08`)
/// keys pass through unchanged.
/// Cache traffic: reads always, writes only when present.
fn cache_segment(read: u64, creation: u64) -> String {
    if creation > 0 {
        format!("cached +{} / {}", short_tokens(creation), short_tokens(read))
    } else {
        format!("cached {}", short_tokens(read))
    }
}

fn agent_breakdowns(row: &AllRow) -> &[AllRow] {
    row.agent_breakdowns.as_deref().unwrap_or(&[])
}

fn visible_models(
    agent: &AllRow,
) -> impl Iterator<Item = &crate::ModelBreakdown> {
    agent.model_breakdowns.iter().filter(|model| {
        model.input_tokens + model.output_tokens + model.cache_read_tokens
            + model.cache_creation_tokens > 0
    })
}

fn humanize_period(period: &str) -> String {
    if period.len() == 10 && period.as_bytes()[4] == b'-' {
        let month = period.get(5..7).and_then(|m| m.parse::<usize>().ok());
        let day = period.get(8..10).unwrap_or("");
        if let Some(month) = month.filter(|m| (1..=12).contains(m)) {
            return format!("{} {}", MONTHS[month - 1], day);
        }
    }
    period.to_string()
}


fn short_tokens(tokens: u64) -> String {
    let value = tokens as f64;
    if tokens >= 1_000_000_000 {
        format!("{:.2}B", value / 1e9)
    } else if tokens >= 1_000_000 {
        let mut text = format!("{:.2}", value / 1e6);
        if text.ends_with("00") {
            text.truncate(text.len() - 3);
        } else if text.ends_with('0') {
            text.truncate(text.len() - 1);
        }
        format!("{text}M")
    } else if tokens >= 1_000 {
        format!("{:.0}k", value / 1e3)
    } else {
        format!("{tokens}")
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn sample_days() -> Vec<SummaryDay> {
        vec![
            SummaryDay {
                label: "Aug 21".into(),
                total_cost: 87.56,
                agents: vec![
                    SummaryAgent {
                        name: "claude".into(),
                        total_cost: 54.52,
                        models: vec![
                            SummaryModel {
                                name: "opus-5".into(),
                                input: 668,
                                output: 197_000,
                                cache_read: 61_750_000,
                                cache_write: 918_000,
                                cost: 44.99,
                            },
                            SummaryModel {
                                name: "sonnet-5".into(),
                                input: 506,
                                output: 190_000,
                                cache_read: 16_240_000,
                                cache_write: 1_750_000,
                                cost: 9.53,
                            },
                        ],
                    },
                    SummaryAgent {
                        name: "fx".into(),
                        total_cost: 31.84,
                        models: vec![
                            SummaryModel {
                                name: "zai/glm-5.2".into(),
                                input: 15_620_000,
                                output: 223_000,
                                cache_read: 12_300_000,
                                cache_write: 0,
                                cost: 25.40,
                            },
                        ],
                    },
                    SummaryAgent {
                        name: "pi".into(),
                        total_cost: 0.0,
                        models: vec![SummaryModel {
                            name: "ox-alpha-free".into(),
                            input: 5_940_000,
                            output: 590_000,
                            cache_read: 193_520_000,
                            cache_write: 0,
                            cost: 0.0,
                        }],
                    },
                ],
            },
            SummaryDay {
                label: "Aug 22".into(),
                total_cost: 1234.56,
                agents: vec![SummaryAgent {
                    name: "prime".into(),
                    total_cost: 1.21,
                    models: vec![SummaryModel {
                        name: "muse-spark-1.2-contributor".into(),
                        input: 10_300_000,
                        output: 169_000,
                        cache_read: 71_600_000,
                        cache_write: 0,
                        cost: 1.21,
                    }],
                }],
            },
        ]
    }

    /// Character column of the first occurrence of `needle`, the way a
    /// terminal counts columns (multi-byte glyphs occupy one cell).
    fn col_of(line: &str, needle: char) -> usize {
        line.chars().position(|ch| ch == needle).expect("needle present")
    }

    #[test]
    fn money_sits_flush_on_the_rail() {
        let lines = summary_body_lines(&sample_days(), 104);
        let money_lines: Vec<&String> =
            lines.iter().filter(|line| line.contains('$')).collect();
        assert!(money_lines.len() >= 9);
        // Fixed width (asserted fully below) plus no trailing padding means
        // every dollar figure ends on exactly the same terminal column.
        for line in &money_lines {
            assert!(
                !line.ends_with(' '),
                "trailing padding breaks the money rail: {line:?}"
            );
            let dollars = line.chars().filter(|ch| *ch == '$').count();
            assert_eq!(dollars, 1, "stray second money figure: {line:?}");
        }
    }

    #[test]
    fn token_columns_align_across_all_model_rows() {
        let lines = summary_body_lines(&sample_days(), 104);
        let model_rows: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains('↑') && line.contains('↓'))
            .collect();
        assert!(model_rows.len() >= 4, "expected model rows");
        let out_cols: Vec<usize> =
            model_rows.iter().map(|line| col_of(line, '↓')).collect();
        assert!(out_cols.iter().all(|col| *col == out_cols[0]), "↓ cols: {out_cols:?}");
        // The strongest layout invariant: every rendered body line occupies
        // exactly `width` display columns, so any column anyone cares about
        // repeats identically down the screen.
        let widths: Vec<usize> = lines
            .iter()
            .filter(|line| !line.is_empty())
            .map(|line| line.chars().count())
            .collect();
        assert!(
            widths.iter().all(|w| *w == 104),
            "line widths vary: {widths:?}"
        );
    }
}
