use std::io::{self, Write};

use crate::{
    style::{Color, TerminalStyle, color},
    terminal::DEFAULT_TERMINAL_WIDTH,
    width::{skip_ansi_escape, truncate_to_width, visible_width, visible_width_max_line},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Align {
    Left,
    Right,
}

pub struct SimpleTable {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Option<Vec<String>>>,
    style: TerminalStyle,
    terminal_width: usize,
    compact_dates: bool,
}

impl SimpleTable {
    pub fn new(headers: Vec<&str>, aligns: Vec<Align>, style: impl Into<TerminalStyle>) -> Self {
        Self {
            headers: headers.into_iter().map(str::to_string).collect(),
            aligns,
            rows: Vec::new(),
            style: style.into(),
            terminal_width: DEFAULT_TERMINAL_WIDTH,
            compact_dates: false,
        }
    }

    pub fn with_terminal_width(mut self, width: usize) -> Self {
        self.terminal_width = width;
        self
    }

    pub fn with_date_compaction(mut self, compact_dates: bool) -> Self {
        self.compact_dates = compact_dates;
        self
    }

    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(Some(row));
    }

    pub fn separator(&mut self) {
        self.rows.push(None);
    }

    pub fn column_count(&self) -> usize {
        self.headers.len()
    }

    pub fn print(&self) -> io::Result<()> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        for line in self.render_lines() {
            writeln!(stdout, "{line}")?;
        }
        Ok(())
    }

    fn render_lines(&self) -> Vec<String> {
        let widths = self.column_widths();
        let mut lines = Vec::new();
        // Borders never stack: an explicit separator landing next to a
        // wrapped-row box (or another separator) collapses into one line.
        let push_border = |lines: &mut Vec<String>, left: char, middle: char, right: char| {
            let already_border = matches!(lines.last(), Some(line) if {
                let trimmed = line.trim_start();
                trimmed.starts_with('├') || trimmed.starts_with('┌') || trimmed.starts_with('└')
            });
            if !already_border {
                lines.push(border(left, middle, right, &widths));
            }
        };
        lines.push(border('┌', '┬', '┐', &widths));
        for header_row in expand_multiline_row(&self.headers, self.headers.len(), &widths) {
            let header_row = header_row
                .iter()
                .map(|header| color(self.style, header, Color::Blue))
                .collect::<Vec<_>>();
            lines.push(table_line(&header_row, &self.aligns, &widths));
        }
        lines.push(border('├', '┼', '┤', &widths));
        for row in self.rows.iter() {
            match row {
                Some(row) => {
                    let row = self.compact_date_row(row, &widths);
                    let row = self.shrink_numeric_cells(row, &widths);
                    // Only genuine content wrapping (Models lists and the
                    // like) boxes a row; single-line rows flow without
                    // separators so reports stay quiet.
                    let content_wraps = row
                        .iter()
                        .skip(1)
                        .any(|cell| cell.contains('\n'));
                    let physical =
                        expand_multiline_row(&row, self.headers.len(), &widths);
                    if content_wraps {
                        push_border(&mut lines, '├', '┼', '┤');
                    }
                    for physical_row in &physical {
                        lines.push(table_line(physical_row, &self.aligns, &widths));
                    }
                    if content_wraps {
                        push_border(&mut lines, '├', '┼', '┤');
                    }
                }
                None => push_border(&mut lines, '├', '┼', '┤'),
            }
        }
        lines.push(border('└', '┴', '┘', &widths));
        lines
    }

    fn column_widths(&self) -> Vec<usize> {
        let content_widths = self
            .headers
            .iter()
            .map(|header| visible_width_max_line(header))
            .collect::<Vec<_>>();
        let mut content_widths = content_widths;
        for row in self.rows.iter().flatten() {
            for (index, cell) in row.iter().enumerate() {
                let cell_width = visible_width_max_line(cell);
                if let Some(width) = content_widths.get_mut(index) {
                    *width = (*width).max(cell_width);
                }
            }
        }
        let widths = content_widths
            .iter()
            .enumerate()
            .map(|(index, width)| {
                if self.aligns.get(index) == Some(&Align::Right) {
                    (width + 3).max(11)
                } else if index == 1 {
                    (width + 2).max(15)
                } else {
                    (width + 2).max(10)
                }
            })
            .collect::<Vec<_>>();
        let total_required = cli_table_required_width(&widths);
        let first_column_min = if self.compact_dates && total_required <= self.terminal_width {
            12
        } else {
            10
        };
        fit_widths_to_terminal(widths, &self.aligns, self.terminal_width, first_column_min)
    }

    fn compact_date_row(&self, row: &[String], widths: &[usize]) -> Vec<String> {
        if !self.compact_dates || widths.first().copied().unwrap_or_default() > 10 {
            return row.to_vec();
        }
        let mut row = row.to_vec();
        if let Some(first) = row.first_mut()
            && let Some(compact) = compact_date_cell(first)
        {
            *first = compact;
        }
        row
    }

    /// Right-aligned numeric cells that no longer fit their column render as
    /// magnitude abbreviations ("68,442,263" -> "68.44M") instead of being
    /// truncated mid-digit, which would silently change the value.
    fn shrink_numeric_cells(&self, row: Vec<String>, widths: &[usize]) -> Vec<String> {
        row.into_iter()
            .enumerate()
            .map(|(index, cell)| {
                let fits = visible_width_max_line(&cell) <= widths.get(index).copied().unwrap_or_default().saturating_sub(2);
                if fits || self.aligns.get(index) != Some(&Align::Right) {
                    return cell;
                }
                shrink_numeric_cell(
                    &cell,
                    widths.get(index).copied().unwrap_or_default().saturating_sub(2),
                )
            })
            .collect()
    }
}

fn expand_multiline_row(row: &[String], column_count: usize, widths: &[usize]) -> Vec<Vec<String>> {
    let cells = (0..column_count)
        .map(|index| {
            let content_width = widths
                .get(index)
                .copied()
                .unwrap_or_default()
                .saturating_sub(2);
            row.get(index)
                .map(|cell| wrap_cell_lines(cell, content_width))
                .filter(|lines| !lines.is_empty())
                .unwrap_or_else(|| vec![String::new()])
        })
        .collect::<Vec<_>>();
    let height = cells.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|line_index| {
            cells
                .iter()
                .map(|lines| lines.get(line_index).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn fit_widths_to_terminal(
    mut widths: Vec<usize>,
    aligns: &[Align],
    terminal_width: usize,
    first_column_min: usize,
) -> Vec<usize> {
    if cli_table_required_width(&widths) <= terminal_width {
        return widths;
    }

    let minimums = widths
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if aligns.get(index) == Some(&Align::Right) {
                10
            } else if index == 0 {
                first_column_min
            } else if index == 1 {
                12
            } else {
                8
            }
        })
        .collect::<Vec<_>>();

    let available_width = terminal_width.saturating_sub(widths.len() + 1);
    let total_content_width = widths.iter().sum::<usize>();
    if total_content_width > 0 {
        let scale = available_width as f64 / total_content_width as f64;
        for (index, width) in widths.iter_mut().enumerate() {
            let scaled = (*width as f64 * scale).floor() as usize;
            *width = scaled.max(minimums[index]);
        }
    }

    while cli_table_required_width(&widths) > terminal_width {
        let Some(index) = widths
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > minimums[*index])
            .max_by_key(|(_, width)| **width)
            .map(|(index, _)| index)
        else {
            break;
        };
        widths[index] -= 1;
    }
    widths
}

fn cli_table_required_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + widths.len() + 1
}

fn wrap_cell_lines(cell: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    for line in cell.lines() {
        if visible_width(line) <= width {
            lines.push(line.to_string());
            continue;
        }
        lines.extend(wrap_cell_line(line, width));
    }
    lines
}

fn wrap_cell_line(line: &str, width: usize) -> Vec<String> {
    if line.split_whitespace().count() <= 1 {
        return vec![truncate_to_width(line, width)];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let candidate_width = if current.is_empty() {
            visible_width(word)
        } else {
            visible_width(&current) + 1 + visible_width(word)
        };
        if candidate_width <= width {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        } else {
            if !current.is_empty() {
                lines.push(current);
            }
            current = if visible_width(word) > width {
                truncate_to_width(word, width)
            } else {
                word.to_string()
            };
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn compact_date_cell(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        Some(format!("{}\n{}", &value[..4], &value[5..]))
    } else {
        None
    }
}

fn table_line(cells: &[String], aligns: &[Align], widths: &[usize]) -> String {
    let mut line = String::from("│");
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(String::as_str).unwrap_or("");
        let align = if index == 0 && cell.starts_with("(assuming ") {
            Align::Right
        } else {
            aligns.get(index).copied().unwrap_or(Align::Left)
        };
        line.push(' ');
        line.push_str(&pad_cell(cell, width.saturating_sub(2), align));
        line.push(' ');
        line.push('│');
    }
    line
}

fn pad_cell(cell: &str, width: usize, align: Align) -> String {
    let visible = visible_width(cell);
    if visible >= width {
        return cell.to_string();
    }
    let padding = width - visible;
    match align {
        Align::Left => format!("{cell}{}", " ".repeat(padding)),
        Align::Right => format!("{}{cell}", " ".repeat(padding)),
    }
}

fn border(left: char, middle: char, right: char, widths: &[usize]) -> String {
    let mut line = String::new();
    line.push(left);
    for (index, width) in widths.iter().enumerate() {
        line.push_str(&"─".repeat(*width));
        line.push(if index + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_date_cell_splits_iso_dates() {
        assert_eq!(
            compact_date_cell("2026-05-18"),
            Some("2026\n05-18".to_string())
        );
        assert_eq!(compact_date_cell("20260518"), None);
    }

    #[test]
    fn width_fitting_keeps_table_within_terminal_when_possible() {
        let widths = fit_widths_to_terminal(
            vec![20, 40, 14, 14],
            &[Align::Left, Align::Left, Align::Right, Align::Right],
            60,
            12,
        );

        assert!(cli_table_required_width(&widths) <= 60);
    }

    #[test]
    fn snapshots_full_table_with_multiline_cells_and_separators() {
        let mut table = SimpleTable::new(
            vec!["Date", "Models", "Input", "Output", "Cost (USD)"],
            vec![
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
            TerminalStyle {
                no_color: true,
                ..TerminalStyle::default()
            },
        )
        .with_terminal_width(120);
        table.push(vec![
            "2026-05-18".to_string(),
            "- claude-sonnet-4\n- gpt-5.2-codex".to_string(),
            "1,234".to_string(),
            "56".to_string(),
            "$0.42".to_string(),
        ]);
        table.push(vec![
            "(assuming cache warmup)".to_string(),
            String::new(),
            "0".to_string(),
            "0".to_string(),
            "$0.00".to_string(),
        ]);
        table.separator();
        table.push(vec![
            "Total".to_string(),
            String::new(),
            "1,234".to_string(),
            "56".to_string(),
            "$0.42".to_string(),
        ]);

        insta::assert_snapshot!(table.render_lines().join("\n"));
    }

    #[test]
    fn snapshots_narrow_table_with_wrapping_truncation_and_compact_dates() {
        let mut table = SimpleTable::new(
            vec!["Date", "Models", "Input", "Output", "Cost (USD)"],
            vec![
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
            TerminalStyle {
                no_color: true,
                ..TerminalStyle::default()
            },
        )
        .with_terminal_width(56)
        .with_date_compaction(true);
        table.push(vec![
            "2026-05-18".to_string(),
            "- claude-sonnet-4-20250514\n- unusually-long-model-name-without-breaks".to_string(),
            "123,456,789".to_string(),
            "9,876,543".to_string(),
            "$12345.67".to_string(),
        ]);

        insta::assert_snapshot!(table.render_lines().join("\n"));
    }

    #[test]
    fn column_widths_uses_max_line_not_sum_for_multiline_cells() {
        let mut table = SimpleTable::new(
            vec!["Date", "Models", "Input", "Output", "Cost (USD)"],
            vec![
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
            TerminalStyle {
                no_color: true,
                ..TerminalStyle::default()
            },
        )
        .with_terminal_width(200);
        // 5 models — a realistic single-agent scenario where the bug would be severe
        table.push(vec![
            "2026-05-18".to_string(),
            "- claude-sonnet-4-20250514 (self-serve)\n- claude-opus-4-5\n- gpt-5.2-codex\n- gemini-3.0-pro-wildly-long\n- claude-haiku-3-5-sonnet".to_string(),
            "1,234".to_string(),
            "56".to_string(),
            "$0.42".to_string(),
        ]);
        let widths = table.column_widths();
        let models_width = widths[1];
        let cell = "- claude-sonnet-4-20250514 (self-serve)\n- claude-opus-4-5\n- gpt-5.2-codex\n- gemini-3.0-pro-wildly-long\n- claude-haiku-3-5-sonnet";
        let widest_line = visible_width_max_line(cell);
        let sum_of_lines = cell.lines().map(visible_width).sum::<usize>();
        // If visible_width_sum were still used, models_width would be ~180
        // With visible_width_max_line, it should be ~widest_line + padding
        assert!(
            models_width < sum_of_lines,
            "Models column width ({models_width}) should be based on widest line ({widest_line}), not sum of all lines ({sum_of_lines})"
        );
        assert!(
            models_width <= widest_line + 3,
            "Models width ({models_width}) should be close to widest line width ({widest_line}), not {sum_of_lines}"
        );
    }
}
/// Rewrites a numeric table cell into its widest magnitude abbreviation that
/// fits `width` display columns. Cells that are not one contiguous number
/// (optionally wrapped in ANSI color escapes) are returned unchanged so the
/// regular wrap and truncate path handles them.
///
/// The abbreviation ladder keeps the most precise representation that fits:
/// full digits, comma-stripped digits, then two/one/zero decimal magnitudes
/// ("68,442,263" -> "68.44M" -> "68.4M" -> "68M").
fn shrink_numeric_cell(cell: &str, width: usize) -> String {
    let Some((prefix, numeric, suffix)) = split_numeric_cell(cell) else {
        return cell.to_string();
    };
    let reserved = visible_width(prefix) + visible_width(suffix);
    let budget = width.saturating_sub(reserved);
    for candidate in abbreviate_number(numeric) {
        if visible_width(&candidate) <= budget {
            return format!("{prefix}{candidate}{suffix}");
        }
    }
    cell.to_string()
}

/// Splits a plain or colored numeric cell into (leading escapes, numeric
/// core, trailing escapes). Returns `None` when the cell's visible content is
/// not exactly one number, leaving the truncate path to handle it.
fn split_numeric_cell(cell: &str) -> Option<(&str, &str, &str)> {
    let bytes = cell.as_bytes();
    if !bytes.contains(&0x1b) {
        let core = numeric_core(cell)?;
        return Some(("", core, ""));
    }
    // Colored cells look like `<esc>...m<number><esc>...m`: escape runs around
    // exactly one visible segment.
    let mut index = 0;
    while index < bytes.len() && bytes[index] == 0x1b {
        index = skip_ansi_escape(bytes, index);
    }
    let text_start = index;
    while index < bytes.len() && bytes[index] != 0x1b {
        index += 1;
    }
    let text_end = index;
    while index < bytes.len() && bytes[index] == 0x1b {
        index = skip_ansi_escape(bytes, index);
    }
    if index != bytes.len() || text_start == text_end {
        return None;
    }
    let core = numeric_core(&cell[text_start..text_end])?;
    Some((&cell[..text_start], core, &cell[text_end..]))
}

/// The longest prefix of `value` that parses as a `$`-optional decimal number
/// with comma groupings, or `None` when the value is not entirely numeric.
fn numeric_core(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed != value {
        return None;
    }
    let body = trimmed.strip_prefix('$').unwrap_or(trimmed);
    let mut seen_digit = false;
    for ch in body.chars() {
        match ch {
            '0'..='9' | ',' => {}
            '.' if seen_digit => {}
            _ => return None,
        }
        if ch.is_ascii_digit() {
            seen_digit = true;
        }
    }
    seen_digit.then_some(trimmed)
}

fn abbreviate_number(value: &str) -> Vec<String> {
    let mut candidates = vec![value.to_string()];
    let Some(number) = value
        .trim_start_matches('$')
        .replace(',', "")
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
    else {
        return candidates;
    };
    let currency = value.starts_with('$');
    let magnitudes: [(&str, f64); 3] = [("B", 1_000_000_000.0), ("M", 1_000_000.0), ("k", 1_000.0)];
    for (unit, divisor) in magnitudes {
        if number < divisor {
            continue;
        }
        let scaled = number / divisor;
        for decimals in [2usize, 1, 0] {
            let mut text = format!("{scaled:.decimals$}");
            if text.contains('.') {
                while text.ends_with('0') {
                    text.pop();
                }
                if text.ends_with('.') {
                    text.pop();
                }
            }
            if currency {
                candidates.push(format!("${text}{unit}"));
            } else {
                candidates.push(format!("{text}{unit}"));
            }
        }
    }
    candidates
}
