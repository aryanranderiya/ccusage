//! GitHub-backed usage backup and sync.
//!
//! Each machine publishes its own aggregate snapshot (`data/<machine>.json`)
//! to a user-owned git repository. Because a machine only ever rewrites its
//! own file, pushes are idempotent and merges are conflict-free by
//! construction: pulling simply unions every machine's per-day rows.

use std::{collections::BTreeMap, fs, process::Command};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Align, Color, Result, SimpleTable,
    cli::SharedArgs,
    cli_error, color, format_currency, format_number, json_float, print_json_or_jq,
    terminal_style, utc_now, format_rfc3339_millis,
};
use crate::adapter::all::SyncUsageRow;

/// Token and cost totals for one date/agent/model cell of one machine.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CellTotals {
    #[serde(rename = "i", default)]
    input_tokens: u64,
    #[serde(rename = "o", default)]
    output_tokens: u64,
    #[serde(rename = "cc", default)]
    cache_creation_tokens: u64,
    #[serde(rename = "cr", default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cost: f64,
}

impl CellTotals {
    fn add_row(&mut self, row: &SyncUsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.cache_creation_tokens += row.cache_creation_tokens;
        self.cache_read_tokens += row.cache_read_tokens;
        self.cost += row.cost;
    }

    fn add_cell(&mut self, cell: &CellTotals) {
        self.input_tokens += cell.input_tokens;
        self.output_tokens += cell.output_tokens;
        self.cache_creation_tokens += cell.cache_creation_tokens;
        self.cache_read_tokens += cell.cache_read_tokens;
        self.cost += cell.cost;
    }

    fn is_empty(&self) -> bool {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
            == 0
            && self.cost == 0.0
    }
}

/// days[date][(agent, model)] -> totals. BTreeMaps keep the JSON stable and
/// diff-friendly across machines and time.
type DayMap = BTreeMap<String, BTreeMap<String, CellTotals>>;

#[derive(Debug, Serialize, Deserialize)]
struct MachineSnapshot {
    version: u32,
    machine: String,
    generated_at: String,
    days: DayMap,
}

/// Agent/model pair encoded into one map key; `\u{1}` cannot appear in agent
/// or model names, so splitting is unambiguous.
fn cell_key(agent: &str, model: &str) -> String {
    format!("{agent}\u{1}{model}")
}

fn day_map_from_rows(rows: &[SyncUsageRow]) -> DayMap {
    let mut days: DayMap = BTreeMap::new();
    for row in rows {
        let key = cell_key(&row.agent, &row.model);
        let entry = days
            .entry(row.date.clone())
            .or_default()
            .entry(key)
            .or_default();
        entry.add_row(row);
    }
    days
}

/// One coalesced row in render order: date, machine, agent.
struct MergedRow {
    date: String,
    machine: String,
    agent: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost: f64,
}

fn agent_of_key(key: &str) -> String {
    key.split('\u{1}')
        .next()
        .unwrap_or(key)
        .to_string()
}

/// Collapses every snapshot into one row per date + machine + agent, summing
/// models together so tables stay one line per agent per day.
fn merged_rows_from_snapshots(snapshots: &[MachineSnapshot]) -> Vec<MergedRow> {
    let mut index =
        std::collections::HashMap::<(String, String, String), CellTotals>::new();
    for snapshot in snapshots {
        for (date, cells) in &snapshot.days {
            for (key, cell) in cells {
                if cell.is_empty() {
                    continue;
                }
                index
                    .entry((
                        date.clone(),
                        snapshot.machine.clone(),
                        agent_of_key(key),
                    ))
                    .or_default()
                    .add_cell(cell);
            }
        }
    }
    let mut rows: Vec<MergedRow> = index
        .into_iter()
        .map(|((date, machine, agent), cell)| MergedRow {
                date,
                machine,
                agent,
                input_tokens: cell.input_tokens,
                output_tokens: cell.output_tokens,
                cache_creation_tokens: cell.cache_creation_tokens,
                cache_read_tokens: cell.cache_read_tokens,
                cost: cell.cost,
            })
        .collect();
    rows.sort_by(|a, b| (&a.date, &a.machine, &a.agent).cmp(&(&b.date, &b.machine, &b.agent)));
    rows
}

pub(crate) fn run_sync(args: crate::cli::SyncArgs) -> Result<()> {
    let shared = &args.shared;
    let repo_spec = args
        .repo
        .clone()
        .or_else(|| std::env::var("CCUSAGE_SYNC_REPO").ok())
        .filter(|repo| !repo.trim().is_empty())
        .ok_or_else(|| {
            cli_error(
                "No sync repository configured. Pass --repo owner/name (or any git URL), or set \
                 CCUSAGE_SYNC_REPO.\nSetup:\n  gh repo create ccusage-sync --private\n  ccusage \
                 sync --repo <you>/ccusage-sync",
            )
        })?;
    let machine = args.machine.clone().unwrap_or_else(default_machine_name);

    // 1. Parse every local agent source into daily agent/model rows.
    let local_rows = crate::adapter::all::collect_sync_rows(shared)?;
    let local_days = day_map_from_rows(&local_rows);

    // 2. Clone or refresh the sync repository.
    let work_dir = sync_work_dir(&repo_spec)?;
    ensure_repo(&work_dir, &repo_spec)?;
    git(&work_dir, &["pull", "--ff-only"]).ok();

    // 3. Publish this machine's snapshot (own file only).
    let snapshot = MachineSnapshot {
        version: 1,
        machine: machine.clone(),
        generated_at: format_rfc3339_millis(utc_now()),
        days: local_days,
    };
    let data_dir = work_dir.join("data");
    fs::create_dir_all(&data_dir)?;
    let own_file = data_dir.join(format!("{}.json", sanitize_machine(&machine)));
    // Rewrite only when the usage data itself changed; a fresh generated_at
    // alone must not produce commits, so repeated syncs stay no-ops.
    let serialized = serde_json::to_string_pretty(&snapshot)?;
    let unchanged = fs::read_to_string(&own_file).is_ok_and(|previous| {
        match (
            serde_json::from_str::<Value>(&previous),
            serde_json::from_str::<Value>(&serialized),
        ) {
            (Ok(mut previous), Ok(mut current)) => {
                if let Some(object) = previous.as_object_mut() {
                    object.remove("generated_at");
                }
                if let Some(object) = current.as_object_mut() {
                    object.remove("generated_at");
                }
                previous == current
            }
            _ => false,
        }
    });
    if unchanged {
        eprintln!("No local changes to publish.");
    } else {
        fs::write(&own_file, &serialized)?;
        if !args.no_push {
            commit_and_push(&work_dir, &machine)?;
        }
    }

    // 4. Load every machine's snapshot and coalesce.
    let mut snapshots = vec![snapshot];
    for entry in fs::read_dir(&data_dir)?.flatten() {
        let path = entry.path();
        if path == own_file
            || path.extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<MachineSnapshot>(&content) {
            Ok(remote) => snapshots.push(remote),
            Err(error) => eprintln!(
                "WARN  Skipping unreadable sync snapshot {}: {error}",
                path.display()
            ),
        }
    }
    snapshots.sort_by(|a, b| a.machine.cmp(&b.machine));
    let machines: Vec<String> = snapshots.iter().map(|s| s.machine.clone()).collect();

    let rows = merged_rows_from_snapshots(&snapshots);
    if crate::wants_json(shared) {
        return print_sync_json(&rows, &machines, shared);
    }
    print_sync_table(rows, &machines, shared, args.by_machine)
}

fn print_sync_json(rows: &[MergedRow], machines: &[String], shared: &SharedArgs) -> Result<()> {
    let mut by_date: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for row in rows {
        if row.input_tokens + row.output_tokens + row.cache_creation_tokens + row.cache_read_tokens
            == 0
            && row.cost == 0.0
        {
            continue;
        }
        by_date.entry(row.date.as_str()).or_default().push(json!({
            "agent": row.agent,
            "machine": row.machine,
            "inputTokens": row.input_tokens,
            "outputTokens": row.output_tokens,
            "cacheCreationTokens": row.cache_creation_tokens,
            "cacheReadTokens": row.cache_read_tokens,
            "cost": json_float(row.cost),
        }));
    }
    let output = json!({
        "machines": machines,
        "days": by_date,
        "totals": totals_value(rows),
    });
    print_json_or_jq(output, shared.jq.as_deref(), shared.no_cost)
}

fn totals_value(rows: &[MergedRow]) -> Value {
    let mut totals = CellTotals::default();
    for row in rows {
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        totals.cache_creation_tokens += row.cache_creation_tokens;
        totals.cache_read_tokens += row.cache_read_tokens;
        totals.cost += row.cost;
    }
    json!({
        "inputTokens": totals.input_tokens,
        "outputTokens": totals.output_tokens,
        "cacheCreationTokens": totals.cache_creation_tokens,
        "cacheReadTokens": totals.cache_read_tokens,
        "totalTokens": totals.input_tokens
            + totals.output_tokens
            + totals.cache_creation_tokens
            + totals.cache_read_tokens,
        "totalCost": json_float(totals.cost),
    })
}

/// Sums machine rows together so the default view shows one line per day and
/// agent across every device.
fn collapse_machines(rows: Vec<MergedRow>) -> Vec<MergedRow> {
    let mut index = std::collections::HashMap::<(String, String), MergedRow>::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for row in rows {
        let key = (row.date.clone(), row.agent.clone());
        match index.get_mut(&key) {
            Some(target) => {
                target.input_tokens += row.input_tokens;
                target.output_tokens += row.output_tokens;
                target.cache_creation_tokens += row.cache_creation_tokens;
                target.cache_read_tokens += row.cache_read_tokens;
                target.cost += row.cost;
            }
            None => {
                order.push(key.clone());
                index.insert(key, row);
            }
        }
    }
    order
        .into_iter()
        .map(|key| index.remove(&key).expect("inserted above"))
        .collect()
}

/// Coalesced view grouped by date. Default merges all machines into one row
/// per agent; `--by-machine` adds a dedicated Machine column instead of
/// hiding which device produced the usage.
fn print_sync_table(
    mut rows: Vec<MergedRow>,
    machines: &[String],
    shared: &SharedArgs,
    by_machine: bool,
) -> Result<()> {
    println!();
    if by_machine {
        rows.sort_by(|a, b| (&a.date, &a.machine, &a.agent).cmp(&(&b.date, &b.machine, &b.agent)));
    } else {
        rows = collapse_machines(rows);
    }
    crate::print_box_title(
        &format!(
            "GitHub Sync Report - {} - {} machine{}",
            machine_label(machines),
            machines.len(),
            if machines.len() == 1 { "" } else { "s" }
        ),
        shared,
    );

    let (headers, aligns): (Vec<&str>, Vec<Align>) = if by_machine {
        (
            vec![
                "Date",
                "Machine",
                "Agent",
                "Input",
                "Output",
                "Cache Read",
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
            ],
        )
    } else {
        (
            vec!["Date", "Agent", "Input", "Output", "Cache Read", "Cost (USD)"],
            vec![
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        )
    };
    let mut table = SimpleTable::new(headers, aligns, terminal_style(shared))
        .with_terminal_width(crate::terminal_width())
        .with_date_compaction(true);
    let mut current_date = String::new();
    for row in &rows {
        let label = if row.date != current_date {
            current_date = row.date.clone();
            row.date.clone()
        } else {
            String::new()
        };
        let mut values = vec![
            label,
            row.agent.clone(),
            format_number(row.input_tokens),
            format_number(row.output_tokens),
            format_number(row.cache_read_tokens),
            format_currency(row.cost),
        ];
        if by_machine {
            values.insert(1, row.machine.clone());
        }
        table.push(values);
    }
    let totals = totals_value(&rows);
    table.separator();
    let yellow = |value: String| color(shared, value, Color::Yellow);
    let mut total_row = vec![
        yellow("Total".to_string()),
        String::new(),
        yellow(format_number(
            totals.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
        )),
        yellow(format_number(
            totals.get("outputTokens").and_then(Value::as_u64).unwrap_or(0),
        )),
        yellow(format_number(
            totals.get("cacheReadTokens").and_then(Value::as_u64).unwrap_or(0),
        )),
        yellow(format_currency(
            totals.get("totalCost").and_then(Value::as_f64).unwrap_or(0.0),
        )),
    ];
    if by_machine {
        total_row.insert(1, String::new());
    }
    table.push(total_row);
    table.print()?;
    Ok(())
}

fn machine_label(machines: &[String]) -> String {
    if machines.len() <= 3 {
        machines.join(", ")
    } else {
        format!("{}, +{} more", machines[..3].join(", "), machines.len() - 3)
    }
}

// --- git plumbing -----------------------------------------------------------

fn repo_url(spec: &str) -> String {
    // owner/name shorthand expands to https; URLs and local paths pass
    // through untouched.
    let trimmed = spec.trim();
    if trimmed.contains("://")
        || trimmed.starts_with("git@")
        || trimmed.starts_with('/')
        || trimmed.starts_with('.')
    {
        return trimmed.to_string();
    }
    match trimmed.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
            format!("https://github.com/{owner}/{name}.git")
        }
        _ => trimmed.to_string(),
    }
}

fn sync_work_dir(repo_spec: &str) -> Result<std::path::PathBuf> {
    let base = crate::home::home_dir()
        .ok_or_else(|| cli_error("home directory is not set"))?
        .join(".local/share/ccusage/sync");
    Ok(base.join(sanitize_repo_name(repo_spec)))
}

fn sanitize(spec: &str) -> String {
    spec.trim()
        .trim_end_matches(".git")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn sanitize_repo_name(spec: &str) -> String {
    sanitize(spec)
}

fn sanitize_machine(machine: &str) -> String {
    sanitize(machine)
}

fn ensure_repo(work_dir: &std::path::Path, repo_spec: &str) -> Result<()> {
    if work_dir.join(".git").is_dir() {
        return Ok(());
    }
    fs::create_dir_all(work_dir)?;
    run_git(
        Command::new("git")
            .arg("clone")
            .arg(repo_url(repo_spec))
            .arg(work_dir),
    )?;
    Ok(())
}

fn git(work_dir: &std::path::Path, git_args: &[&str]) -> Result<String> {
    run_git(Command::new("git").args(git_args).current_dir(work_dir))
}

fn run_git(command: &mut Command) -> Result<String> {
    let output = command.output().map_err(|error| {
        cli_error(format!(
            "failed to run git ({error}). Install git to use ccusage sync."
        ))
    })?;
    if !output.status.success() {
        return Err(cli_error(format!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn commit_and_push(work_dir: &std::path::Path, machine: &str) -> Result<()> {
    git(work_dir, &["add", "-A"])?;
    let staged = std::process::Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(work_dir)
        .status()
        .map_err(|error| cli_error(format!("failed to run git ({error})")))?;
    if !staged.success() {
        git(
            work_dir,
            &[
                "-c",
                "user.name=ccusage",
                "-c",
                "user.email=ccusage@localhost",
                "commit",
                "-m",
                &format!("sync: {machine}"),
            ],
        )?;
    }
    // Push; on rejection (someone pushed first), rebase onto remote and retry.
    let push_status = std::process::Command::new("git")
        .args(["push", "origin", "HEAD"])
        .current_dir(work_dir)
        .status()
        .map_err(|error| cli_error(format!("failed to run git ({error})")))?;
    if !push_status.success() {
        git(work_dir, &["pull", "--rebase"])?;
        run_git(
            Command::new("git")
                .args(["push", "origin", "HEAD"])
                .current_dir(work_dir),
        )?;
    }
    Ok(())
}

fn default_machine_name() -> String {
    if let Some(name) = std::env::var("HOSTNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    Command::new("hostname")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown-machine".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(machine: &str, days: DayMap) -> MachineSnapshot {
        MachineSnapshot {
            version: 1,
            machine: machine.to_string(),
            generated_at: "2026-08-22T00:00:00.000Z".to_string(),
            days,
        }
    }

    fn cell(input: u64, output: u64) -> CellTotals {
        CellTotals {
            input_tokens: input,
            output_tokens: output,
            ..CellTotals::default()
        }
    }

    #[test]
    fn repo_shorthand_expands_to_github_https() {
        assert_eq!(
            repo_url("me/ccusage-sync"),
            "https://github.com/me/ccusage-sync.git"
        );
        assert_eq!(
            repo_url("git@github.com:me/ccusage-sync.git"),
            "git@github.com:me/ccusage-sync.git"
        );
        assert_eq!(
            repo_url("https://example.com/repo.git"),
            "https://example.com/repo.git"
        );
    }

    #[test]
    fn merged_rows_add_across_machines_and_collapse_models_per_agent() {
        let mut first_day = BTreeMap::new();
        let mut second_day = BTreeMap::new();
        first_day.insert(cell_key("pi", "ox-alpha-free"), cell(100, 10));
        first_day.insert(cell_key("pi", "[pi] muse"), cell(50, 5));
        // Same agent on a second model collapses into one row per machine.
        second_day.insert(cell_key("fx", "zai/glm-5.2"), cell(200, 20));

        let rows = merged_rows_from_snapshots(&[
            snapshot("laptop", [("2026-08-20".to_string(), first_day)].into()),
            snapshot("desktop", [("2026-08-20".to_string(), second_day)].into()),
        ]);

        assert_eq!(rows.len(), 2);
        let pi = rows
            .iter()
            .find(|row| row.agent == "pi")
            .expect("pi row present");
        assert_eq!(pi.machine, "laptop");
        assert_eq!(pi.input_tokens, 150, "models of one agent sum together");
        assert_eq!(rows[0].date, "2026-08-20");
    }

    #[test]
    fn day_map_round_trips_through_cell_keys() {
        let row = SyncUsageRow {
            date: "2026-08-21".to_string(),
            agent: "prime".to_string(),
            model: "muse-spark-1.2-contributor".to_string(),
            input_tokens: 7,
            output_tokens: 8,
            cache_creation_tokens: 9,
            cache_read_tokens: 10,
            cost: 0.25,
        };
        let days = day_map_from_rows(&[row]);
        let stored = &days["2026-08-21"][&cell_key("prime", "muse-spark-1.2-contributor")];
        assert_eq!(stored.input_tokens, 7);
        assert_eq!(stored.cost, 0.25);
    }
}
