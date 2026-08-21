use std::io::IsTerminal;

mod loader;
mod report;
mod types;

use ccusage_adapter_codex::CodexGroup;
#[cfg(test)]
use ccusage_adapter_codex::CodexModelUsage;
use ccusage_adapter_common::filter_loaded_entries_by_date;
use ccusage_core::*;

mod adapter {
    pub use ccusage_adapter_amp as amp;
    pub use ccusage_adapter_claude as claude;
    pub use ccusage_adapter_codebuff as codebuff;
    pub use ccusage_adapter_codex as codex;
    pub use ccusage_adapter_copilot as copilot;
    pub use ccusage_adapter_droid as droid;
    pub use ccusage_adapter_fx as fx;
    pub use ccusage_adapter_gemini as gemini;
    pub use ccusage_adapter_goose as goose;
    pub use ccusage_adapter_grok as grok;
    pub use ccusage_adapter_hermes as hermes;
    pub use ccusage_adapter_kilo as kilo;
    pub use ccusage_adapter_kimi as kimi;
    pub use ccusage_adapter_openclaw as openclaw;
    pub use ccusage_adapter_opencode as opencode;
    pub use ccusage_adapter_pi as pi;
    pub use ccusage_adapter_prime as prime;
    pub use ccusage_adapter_qwen as qwen;
}

use crate::{
    Result,
    cli::{AgentCommandArgs, AgentReportKind, SharedArgs},
    print_json_or_jq, wants_json,
};

/// One flattened daily usage cell: a single date, agent, and model with its
/// token and cost totals. The sync feature aggregates every loaded source
/// into these rows so per-machine snapshots stay small and mergeable.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncUsageRow {
    pub date: String,
    pub agent: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost: f64,
}

/// Loads every detected agent source and flattens the daily report into
/// per-date/per-agent/per-model rows. Cost semantics match the daily report
/// (auto mode: embedded display cost when present, otherwise priced).
pub fn collect_sync_rows(shared: &SharedArgs) -> Result<Vec<SyncUsageRow>> {
    
    let result = loader::load_rows(AgentReportKind::Daily, shared)?;
    let mut rows = Vec::new();
    for row in &result.rows {
        let Some(breakdowns) = row.agent_breakdowns.as_ref() else {
            continue;
        };
        for agent in breakdowns {
            for model in &agent.model_breakdowns {
                if model.input_tokens
                    + model.output_tokens
                    + model.cache_creation_tokens
                    + model.cache_read_tokens
                    + model.extra_total_tokens
                    == 0
                {
                    continue;
                }
                rows.push(SyncUsageRow {
                    date: row.period.clone(),
                    agent: agent.agent.to_string(),
                    model: model.model_name.clone(),
                    input_tokens: model.input_tokens,
                    output_tokens: model.output_tokens,
                    cache_creation_tokens: model.cache_creation_tokens,
                    cache_read_tokens: model.cache_read_tokens,
                    cost: model.cost,
                });
            }
        }
    }
    Ok(rows)
}

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let kind = args.kind;
    let shared = args.shared;
    let include_agents = args.by_agent;
    if let Some(sections) = args.sections {
        let sections = requested_sections(kind, sections);
        let result = loader::load_sections(&sections, &shared)?;
        if wants_json(&shared) {
            return report::print_sections_report_json(
                &result.sections,
                kind,
                include_agents,
                shared.jq.as_deref(),
                shared.no_cost,
            );
        }
        for (section_kind, rows) in &result.sections {
            report::print_table(
                rows,
                *section_kind,
                &shared,
                result.detected_agents_for(*section_kind),
            )?;
        }
        return Ok(());
    }
    let result = loader::load_rows(kind, &shared)?;
    if wants_json(&shared) {
        let output = report::report_json_with_agents(&result.rows, kind, include_agents);
        return print_json_or_jq(output, shared.jq.as_deref(), shared.no_cost);
    }
    // Interactive terminals get the grouped summary view by default; pipes,
    // scripts, --breakdown, and section lists keep the full tables.
    let interactive = !shared.breakdown
        && args.sections.is_none()
        && matches!(kind, AgentReportKind::Daily)
        && std::io::stdout().is_terminal();
    if interactive {
        return report::print_summary_view(
            &result.rows,
            kind,
            &shared,
            &result.detected_agents,
        );
    }
    report::print_table(&result.rows, kind, &shared, &result.detected_agents)
}

fn requested_sections(
    command_kind: AgentReportKind,
    sections: Vec<AgentReportKind>,
) -> Vec<AgentReportKind> {
    let mut requested = vec![command_kind];
    for section in [
        AgentReportKind::Daily,
        AgentReportKind::Weekly,
        AgentReportKind::Monthly,
        AgentReportKind::Session,
    ] {
        if section != command_kind && sections.contains(&section) {
            requested.push(section);
        }
    }
    requested
}

#[cfg(test)]
use loader::{aggregate_rows, codex_group_row, load_agent_rows_parallel, load_rows, load_sections};
#[cfg(test)]
use report::{
    all_report_title, all_table_columns, all_table_row, report_json, report_json_with_agents,
    sections_report_json,
};
#[cfg(test)]
use types::{AgentLoadSpec, AgentRows, AllRow};

#[cfg(test)]
mod tests;
