//! What the lab prints: one compact block per utterance, a summary table
//! across them, and the same as JSON for machines.

use std::fmt::Write as _;

use serde_json::{json, Value};

use crate::lab::{Run, Speech, StepSum};

const BAR: usize = 24;

fn paint(on: bool, code: &str, text: &str) -> String {
    if on {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn bar(ms: f64, scale: f64) -> String {
    // Waits under a millisecond are all alike: no bars.
    let n = if scale < 1.0 {
        0
    } else {
        ((ms / scale) * BAR as f64).round() as usize
    };
    "█".repeat(n.min(BAR))
}

fn step_line(step: &StepSum) -> String {
    let model = step
        .model
        .as_deref()
        .map_or(String::new(), |m| format!(" [{m}]"));
    let runs = if step.runs > 1 {
        format!(" x{}", step.runs)
    } else {
        String::new()
    };
    let skipped = if step.skipped.is_empty() {
        String::new()
    } else {
        format!("  SKIPPED {}", step.skipped.join(","))
    };
    format!("{} {} ms{runs}{model}{skipped}", step.task, step.ms)
}

/// `2/5`, or just `2` when the count is not known (a microphone).
fn position(index: usize, total: usize) -> String {
    if total == 0 {
        index.to_string()
    } else {
        format!("{index}/{total}")
    }
}

/// One utterance: what was heard, then each graph's text and where the wait
/// after the stop went.
pub fn utterance(
    index: usize,
    total: usize,
    speech: &Speech,
    runs: &[(String, Run)],
    color: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        paint(
            color,
            "1",
            &format!("── {} {}", position(index, total), speech.id)
        )
    );
    if let Some(reference) = &speech.reference {
        let _ = writeln!(out, "   said   {reference}");
    }
    let _ = writeln!(out, "   heard  {}", speech.heard());
    let width = runs.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let scale = runs.iter().map(|(_, r)| r.dead_air_ms).fold(0.0, f64::max);
    for (label, run) in runs {
        let wait = format!("{:>5.0} ms", run.dead_air_ms);
        let wait = paint(color, "33", &wait);
        let _ = writeln!(
            out,
            "   {label:<width$}  {wait}  {:<BAR$}  asr {:.0} + graph {:.0}",
            bar(run.dead_air_ms, scale),
            run.asr_tail_ms,
            run.graph_ms,
        );
        let _ = writeln!(out, "   {:<width$}  => {}", "", run.typed);
        for step in &run.steps {
            let line = step_line(step);
            let line = if step.skipped.is_empty() {
                line
            } else {
                paint(color, "31", &line)
            };
            let _ = writeln!(out, "   {:<width$}     {line}", "");
        }
    }
    out
}

/// What the lab has measured per graph, across utterances.
#[derive(Default)]
pub struct Summary {
    asr: Vec<f64>,
    rows: Vec<Row>,
}

struct Row {
    label: String,
    dead: Vec<f64>,
    graph: Vec<f64>,
    skipped: usize,
    asked: usize,
}

/// Nearest-rank percentile; `p` in 0..=1.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

impl Summary {
    pub fn add(&mut self, runs: &[(String, Run)]) {
        if let Some((_, first)) = runs.first() {
            self.asr.push(first.asr_tail_ms);
        }
        for (label, run) in runs {
            let at = self
                .rows
                .iter()
                .position(|r| &r.label == label)
                .unwrap_or_else(|| {
                    self.rows.push(Row {
                        label: label.clone(),
                        dead: Vec::new(),
                        graph: Vec::new(),
                        skipped: 0,
                        asked: 0,
                    });
                    self.rows.len() - 1
                });
            let row = &mut self.rows[at];
            row.dead.push(run.dead_air_ms);
            row.graph.push(run.graph_ms);
            row.asked += run.steps.iter().map(|s| s.runs).sum::<usize>();
            row.skipped += run.steps.iter().map(|s| s.skipped.len()).sum::<usize>();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn table(&self) -> String {
        let width = self
            .rows
            .iter()
            .map(|r| r.label.len())
            .max()
            .unwrap_or(5)
            .max(5);
        let n = self.rows.first().map_or(0, |r| r.dead.len());
        let mut out = String::new();
        let _ = writeln!(
            out,
            "summary: {n} utterance{}, speech model finished {:.0} ms (p50) / {:.0} ms (p95) after stop",
            if n == 1 { "" } else { "s" },
            percentile(&self.asr, 0.5),
            percentile(&self.asr, 0.95),
        );
        let _ = writeln!(
            out,
            "{:<width$}  {:>9} {:>9}  {:>9} {:>9}  {:>7}",
            "graph", "wait p50", "wait p95", "graph p50", "graph p95", "skipped"
        );
        for row in &self.rows {
            let _ = writeln!(
                out,
                "{:<width$}  {:>7.0}ms {:>7.0}ms  {:>7.0}ms {:>7.0}ms  {:>3}/{:<3}",
                row.label,
                percentile(&row.dead, 0.5),
                percentile(&row.dead, 0.95),
                percentile(&row.graph, 0.5),
                percentile(&row.graph, 0.95),
                row.skipped,
                row.asked,
            );
        }
        out
    }

    pub fn json(&self) -> Value {
        json!({
            "asr_tail_ms": {"p50": percentile(&self.asr, 0.5), "p95": percentile(&self.asr, 0.95)},
            "graphs": self.rows.iter().map(|r| json!({
                "graph": r.label,
                "n": r.dead.len(),
                "dead_air_ms": {"p50": percentile(&r.dead, 0.5), "p95": percentile(&r.dead, 0.95)},
                "graph_ms": {"p50": percentile(&r.graph, 0.5), "p95": percentile(&r.graph, 0.95)},
                "steps_skipped": r.skipped,
                "steps_run": r.asked,
            })).collect::<Vec<_>>(),
        })
    }
}

/// One utterance for `--json`.
pub fn utterance_json(speech: &Speech, runs: &[(String, Run)]) -> Value {
    json!({
        "id": speech.id,
        "reference": speech.reference,
        "heard": speech.heard(),
        "graphs": runs.iter().map(|(label, run)| json!({
            "graph": label,
            "text": run.typed,
            "asr_tail_ms": run.asr_tail_ms,
            "graph_ms": run.graph_ms,
            "dead_air_ms": run.dead_air_ms,
            "steps": run.steps.iter().map(|s| json!({
                "task": s.task,
                "model": s.model,
                "ms": s.ms,
                "runs": s.runs,
                "skipped": s.skipped,
            })).collect::<Vec<_>>(),
            "pieces": run.pieces.iter().map(|p| json!({
                "heard": p.heard,
                "typed": p.typed,
                "start_ms": p.start_ms,
                "ms": p.ms,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, asr: f64, graph: f64, steps: Vec<StepSum>) -> Run {
        Run {
            typed: text.into(),
            pieces: Vec::new(),
            steps,
            asr_tail_ms: asr,
            graph_ms: graph,
            dead_air_ms: asr + graph,
        }
    }

    fn speech() -> Speech {
        Speech::typed("clip.wav".into(), "Um hello.")
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let v = [50.0, 10.0, 40.0, 20.0, 30.0];
        assert_eq!(percentile(&v, 0.5), 30.0);
        assert_eq!(percentile(&v, 0.95), 50.0);
        assert_eq!(percentile(&v, 0.0), 10.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }

    #[test]
    fn a_block_shows_text_wait_split_and_skips() {
        let step = StepSum {
            task: "text.cleanup".into(),
            model: Some("share/m".into()),
            ms: 231,
            runs: 2,
            skipped: vec!["new-word".into()],
        };
        let runs = vec![
            ("exact".to_owned(), run("Um hello.", 400.0, 0.0, vec![])),
            ("tidy".to_owned(), run("Hello.", 400.0, 200.0, vec![step])),
        ];
        let text = utterance(1, 3, &speech(), &runs, false);
        assert!(text.contains("── 1/3 clip.wav"), "{text}");
        assert!(text.contains("heard  Um hello."), "{text}");
        assert!(text.contains("exact    400 ms"), "{text}");
        assert!(text.contains("tidy     600 ms"), "{text}");
        assert!(text.contains("asr 400 + graph 200"), "{text}");
        assert!(text.contains("=> Hello."), "{text}");
        assert!(
            text.contains("text.cleanup 231 ms x2 [share/m]  SKIPPED new-word"),
            "{text}"
        );
        assert!(!text.contains('\x1b'));
        // The longest wait fills the bar, a shorter one less of it.
        let bars: Vec<usize> = text.lines().map(|l| l.matches('█').count()).collect();
        assert_eq!(bars.iter().max(), Some(&BAR));
        assert!(bars.contains(&16));
        assert!(utterance(1, 1, &speech(), &runs, true).contains('\x1b'));
    }

    #[test]
    fn no_wait_draws_no_bar() {
        assert_eq!(bar(0.2, 0.3), "");
        assert_eq!(bar(5.0, 10.0).chars().count(), BAR / 2);
    }

    #[test]
    fn an_unknown_count_shows_only_the_position() {
        assert_eq!(position(2, 0), "2");
        assert_eq!(position(2, 5), "2/5");
    }

    #[test]
    fn the_summary_gives_p50_and_p95_per_graph() {
        let mut summary = Summary::default();
        for (asr, graph) in [(100.0, 10.0), (200.0, 30.0), (300.0, 20.0)] {
            summary.add(&[
                ("exact".to_owned(), run("a", asr, 0.0, vec![])),
                (
                    "tidy".to_owned(),
                    run(
                        "a",
                        asr,
                        graph,
                        vec![StepSum {
                            task: "t".into(),
                            model: None,
                            ms: 1,
                            runs: 1,
                            skipped: vec!["timeout".into()],
                        }],
                    ),
                ),
            ]);
        }
        let table = summary.table();
        assert!(table.contains("3 utterances"), "{table}");
        assert!(table.contains("200 ms (p50) / 300 ms (p95)"), "{table}");
        let tidy = table.lines().find(|l| l.starts_with("tidy")).unwrap();
        assert!(tidy.contains("230ms"), "{tidy}");
        assert!(tidy.contains("320ms"), "{tidy}");
        assert!(tidy.contains("3/3"), "{tidy}");
        let json = summary.json();
        assert_eq!(json["graphs"][1]["graph_ms"]["p95"], 30.0);
        assert!(!summary.is_empty());
    }

    #[test]
    fn json_carries_every_measure() {
        let runs = vec![("tidy".to_owned(), run("Hello.", 1.0, 2.0, vec![]))];
        let v = utterance_json(&speech(), &runs);
        assert_eq!(v["graphs"][0]["dead_air_ms"], 3.0);
        assert_eq!(v["graphs"][0]["text"], "Hello.");
        assert_eq!(v["heard"], "Um hello.");
    }
}
