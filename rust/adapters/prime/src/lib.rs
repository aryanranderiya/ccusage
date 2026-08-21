use std::path::PathBuf;

use ccusage_adapter_common::filter_loaded_entries_by_date;
use ccusage_core::*;

mod paths;

use crate::{
    Result, cli::AgentCommandArgs, cli::AgentReportKind, print_json_or_jq, print_usage_table,
    sort_summaries, wants_json,
};

pub use paths::paths as default_paths;

/// Prime-agent sessions are pi-format session stores; loading, dedupe,
/// cost semantics, and summaries delegate to the pi adapter's named-store
/// surface so the two sources never drift.
pub fn load_entries(
    shared: &ccusage_core::cli::SharedArgs,
    custom_path: Option<&str>,
    pricing: Option<&PricingMap>,
) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(crate::progress::UsageLoadAgent("prime"), shared.json, || {
        let store_paths: Vec<PathBuf> = paths::paths(custom_path)?;
        ccusage_adapter_pi::load_entries_for_store_paths(shared, store_paths, "prime", pricing)
    })
}

pub fn has_data() -> bool {
    paths::paths(None)
        .is_ok_and(|dirs| dirs.iter().any(|dir| dir.is_dir()))
}

pub fn summarize_entries(
    entries: &[LoadedEntry],
    kind: AgentReportKind,
) -> Result<Vec<crate::UsageSummary>> {
    ccusage_adapter_pi::summarize_entries(entries, kind)
}
pub fn run(args: AgentCommandArgs) -> Result<()> {
    let pricing = crate::PricingMap::load_with_overrides(
        args.shared.offline,
        crate::log_level() != Some(0),
        args.shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&args.shared, args.prime_path.as_deref(), Some(&pricing))?;
    filter_loaded_entries_by_date(&mut entries, &args.shared);
    let mut rows = summarize_entries(&entries, args.kind)?;
    sort_summaries(&mut rows, &args.shared.order, |row| {
        ccusage_core::summary_period(row)
    });
    if wants_json(&args.shared) {
        let report = ccusage_adapter_pi::report_from_rows(&rows, args.kind);
        return print_json_or_jq(report, args.shared.jq.as_deref(), args.shared.no_cost);
    }
    print_usage_table(
        "Prime Agent Token Usage Report",
        ccusage_core::first_column(args.kind),
        &rows,
        &args.shared,
        false,
        None,
    )?;
    Ok(())
}
