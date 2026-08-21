use std::{
    fs,
    path::Path,
    sync::Arc,
};

use jiff::tz::TimeZone as JiffTimeZone;
use serde::Deserialize;

use crate::{
    LoadedEntry, PricingMap, Result, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_from_pricing, cli::CostMode, fast::LinePrefilter, format_date_tz,
    missing_pricing_model_for_usage,
};
use ccusage_adapter_common::jsonl;

/// A single parsed fx usage.jsonl record. Only the fields ccusage consumes are
/// declared; serde skips everything else.
#[derive(Debug, Deserialize)]
struct FxLine {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    kind: Option<String>,
    // `coverage` and `incident` records have no `fact`; only `generation`
    // records carry token usage.
    fact: Option<FxGeneration>,
}

/// The `fact` block carried by a `generation` record.
#[derive(Debug, Deserialize)]
struct FxGeneration {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    created_at_ms: u64,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    output_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cache_read_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cache_write_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_f64")]
    total_cost: Option<f64>,
}

/// One discovered fx session: id plus start time, used to attribute ledger
/// generations to sessions without double counting.
#[derive(Debug)]
pub(super) struct FxSession {
    pub id: String,
    pub project: Option<String>,
    pub started_at_ms: u64,
}

/// Reads one fx data directory: the authoritative root `usage.jsonl` ledger
/// provides every generation exactly once, and the session index under
/// `sessions/` attributes each generation to a session and workspace.
pub fn read_data_dir(
    data_root: &Path,
    tz: Option<&JiffTimeZone>,
    mode: CostMode,
    pricing: Option<&PricingMap>,
) -> Result<Vec<LoadedEntry>> {
    let usage_file = data_root.join("usage.jsonl");
    let content = fs::read(&usage_file)?;
    let sessions = load_session_index(data_root);
    let fallback_project = Arc::<str>::from(data_root.file_name().and_then(|name| name.to_str()).unwrap_or("fx"));
    let no_session = Arc::<str>::from("(no session)");

    // Only `generation` records carry token counts under a `fact` key, so
    // require both substrings before JSON parsing.
    let prefilter = LinePrefilter::all(&[br#""generation""#, br#""fact""#]);
    let mut entries = Vec::new();
    for record in jsonl::records::<FxLine>(&content, Some(&prefilter)) {
        if record.kind.as_deref() != Some("generation") {
            continue;
        }
        let Some(fact) = record.fact else {
            continue;
        };
        if fact.created_at_ms == 0 {
            continue;
        }
        let (session_id, project) = attribute_generation(&fact, &sessions, &fallback_project, &no_session);
        if let Some(entry) = loaded_entry(fact, session_id, project, tz, mode, pricing) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Prices usage by the model id exactly as the source wrote it, bypassing
/// the "[fx] "-prefixed display label.
fn calculate_cost_from_tokens_raw(
    raw_model: Option<&str>,
    usage: TokenUsageRaw,
    mode: CostMode,
    pricing: Option<&PricingMap>,
) -> f64 {
    if mode == CostMode::Display || raw_model.is_none() {
        return 0.0;
    }
    let Some(pricing) = pricing else {
        return 0.0;
    };
    pricing
        .find(raw_model.expect("checked above"))
        .map(|pricing| calculate_cost_from_pricing(usage, pricing))
        .unwrap_or(0.0)
}

fn loaded_entry(
    fact: FxGeneration,
    session_id: Arc<str>,
    project: Arc<str>,
    tz: Option<&JiffTimeZone>,
    mode: CostMode,
    pricing: Option<&PricingMap>,
) -> Option<LoadedEntry> {
    let usage = TokenUsageRaw {
        input_tokens: fact.input_tokens,
        output_tokens: fact.output_tokens,
        cache_creation_input_tokens: fact.cache_write_tokens,
        cache_read_input_tokens: fact.cache_read_tokens,
        speed: None,
        cache_creation: None,
    };
    if crate::total_usage_tokens(usage) == 0 {
        return None;
    }
    let raw_model = fact.model.clone();
    let model = raw_model
        .as_ref()
        .map(|model| format!("[fx] {model}"));
    // fx's own ledger reports $0 for subscription-covered generations. In
    // auto mode a zero embedded cost falls through to LiteLLM rates so the
    // report still shows what the tokens would cost; --mode display keeps
    // fx's number and --mode calculate always prices from scratch.
    let display_cost = match mode {
        CostMode::Auto => fact.total_cost.filter(|cost| *cost > 0.0),
        CostMode::Display | CostMode::Calculate => fact.total_cost,
    };
    // Price lookups use the raw model id ("zai/glm-5.2"); only the rendered
    // label carries the [fx] prefix.
    let cost = match mode {
        CostMode::Display => display_cost.unwrap_or(0.0),
        CostMode::Auto if display_cost.is_some() => {
            display_cost.expect("checked above")
        }
        _ => calculate_cost_from_tokens_raw(raw_model.as_deref(), usage, mode, pricing),
    };
    let missing_pricing_model = missing_pricing_model_for_usage(
        raw_model.as_deref(),
        usage,
        display_cost,
        mode,
        pricing,
    );
    let timestamp = crate::TimestampMs::from_millis(fact.created_at_ms as i64);
    let timestamp_text = crate::format_rfc3339_millis(timestamp);
    let data = UsageEntry {
        session_id: Some(session_id.to_string()),
        timestamp: timestamp_text,
        version: None,
        message: UsageMessage {
            usage,
            model: model.clone(),
            id: fact.id.clone(),
        },
        cost_usd: display_cost,
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
    };
    Some(LoadedEntry {
        date: format_date_tz(timestamp, tz),
        timestamp,
        project: project.clone(),
        session_id,
        project_path: project,
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model,
        data,
        usage_limit_reset_time: None,
        missing_pricing_model,
    })
}

/// Attributes a generation to the most recently started session at or before
/// its timestamp. Sessions can overlap when several fx agents run in parallel,
/// so "latest start wins" keeps attribution deterministic while every
/// generation is still counted exactly once.
fn attribute_generation<'a>(
    fact: &FxGeneration,
    sessions: &'a [FxSession],
    fallback_project: &Arc<str>,
    no_session: &Arc<str>,
) -> (Arc<str>, Arc<str>) {
    let mut best: Option<&FxSession> = None;
    for session in sessions {
        if session.started_at_ms <= fact.created_at_ms
            && best.is_none_or(|current| session.started_at_ms > current.started_at_ms)
        {
            best = Some(session);
        }
    }
    match best {
        Some(session) => (
            Arc::<str>::from(session.id.as_str()),
            session
                .project
                .as_deref()
                .map(|project| Arc::<str>::from(project))
                .unwrap_or_else(|| fallback_project.clone()),
        ),
        None => (no_session.clone(), fallback_project.clone()),
    }
}

/// Walks `<root>/sessions/*/events.jsonl` and collects each session's id,
/// start time, and workspace root. Only `session_started` records are read,
/// so multi-hundred-MiB event logs cost one prefiltered pass.
fn load_session_index(data_root: &Path) -> Vec<FxSession> {
    let sessions_root = data_root.join("sessions");
    let Ok(read_dir) = fs::read_dir(&sessions_root) else {
        return Vec::new();
    };
    let mut session_dirs = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            session_dirs.push(path);
        }
    }
    session_dirs.sort();
    let mut sessions = Vec::new();
    let prefilter = LinePrefilter::all(&[br#""session_started""#]);
    for dir in session_dirs {
        let events = dir.join("events.jsonl");
        let Ok(content) = fs::read(&events) else {
            continue;
        };
        for record in jsonl::records::<FxEventLine>(&content, Some(&prefilter)) {
            let Some(started) = record.payload else {
                continue;
            };
            let Some(id) = started.id else {
                continue;
            };
            let started_at_ms = started.created_at_ms;
            if started_at_ms == 0 {
                continue;
            }
            sessions.push(FxSession {
                id,
                project: started.workspace_root,
                started_at_ms,
            });
        }
    }
    sessions.sort_by_key(|session| session.started_at_ms);
    sessions
}

/// A `session_started` event line; only the identity fields ccusage needs are
/// declared.
#[derive(Debug, Deserialize)]
struct FxEventLine {
    payload: Option<FxSessionStarted>,
}

#[derive(Debug, Deserialize)]
struct FxSessionStarted {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    created_at_ms: u64,
    #[serde(rename = "workspace_root", default, deserialize_with = "jsonl::non_empty_string")]
    workspace_root: Option<String>,
}

/// Stable dedupe identity. fx generation facts carry globally unique ids
/// (`gen_...`), so the id alone dedupes a ledger re-read or overlapping data
/// directories; entries without an id fall back to the full tuple.
pub(super) fn entry_id(entry: &LoadedEntry) -> String {
    if let Some(id) = entry.data.message.id.as_deref().filter(|id| !id.is_empty()) {
        return format!("fx:{id}");
    }
    [
        "fx",
        entry.data.message.id.as_deref().unwrap_or_default(),
        entry.project.as_ref(),
        entry.session_id.as_ref(),
        entry.data.timestamp.as_str(),
        &entry.data.message.usage.input_tokens.to_string(),
        &entry.data.message.usage.output_tokens.to_string(),
        &entry.data.message.usage.cache_creation_input_tokens.to_string(),
        &entry.data.message.usage.cache_read_input_tokens.to_string(),
    ]
    .join(":")
}
