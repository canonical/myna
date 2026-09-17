//! Hermetic terminal-output tests (feature 011-accessible-dictation-ux, US5,
//! FR-028/029, `contracts/terminal-output.md` T1-T3).
//!
//! `myna-cli` is a `[[bin]]`-only crate (no library target), so these tests
//! exercise the pure `myna_orchestrator::render_event_line` function
//! `StdoutSink` is a thin I/O wrapper over (see `sink.rs`'s doc comment) —
//! the same split this codebase uses throughout (e.g. `accent.js`'s pure
//! resolvers vs. `SystemPreferences`, `hudLogic.js` vs. `hud.js`). No real
//! stdout capture, no subprocess, no audio/network — purely a function of
//! `OrchestratorEvent -> Option<RenderedLine>`.

use myna_orchestrator::{render_event_line, OrchestratorEvent, OutputStream};

/// No `\x1b` (ESC, the start of every ANSI escape/cursor-movement/colour
/// sequence) and no bare `\r` (carriage return, the in-place-redraw
/// mechanism) anywhere in a rendered line.
fn is_plain_line(text: &str) -> bool {
    !text.contains('\x1b') && !text.contains('\r')
}

/// A rendered line's `[marker]` prefix, if it has one in the expected shape.
fn marker_of(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('[')?;
    let (marker, _) = rest.split_once(']')?;
    Some(marker)
}

// ── T1/T072: every event that produces output carries an explicit textual
//    marker, distinguishable with colour/emoji removed, and no ANSI codes ──

#[test]
fn every_state_transition_and_result_line_has_an_explicit_textual_marker() {
    let events = [
        OrchestratorEvent::Loading,
        OrchestratorEvent::Ready,
        OrchestratorEvent::Snippet("partial text".into()),
        OrchestratorEvent::Final("committed text".into()),
        OrchestratorEvent::Unstable("unstable text".into()),
        OrchestratorEvent::Done(String::new()),
        OrchestratorEvent::Done("hello world".into()),
        OrchestratorEvent::Error {
            code: "some_code".into(),
            message: "some detail".into(),
        },
    ];

    for event in &events {
        let line = render_event_line(event)
            .unwrap_or_else(|| panic!("{event:?} must produce a rendered line"));
        assert!(
            marker_of(&line.text).is_some(),
            "{event:?} rendered {:?} without a leading [marker]",
            line.text
        );
        assert!(
            is_plain_line(&line.text),
            "{event:?} rendered {:?} containing an ANSI/cursor-movement sequence",
            line.text
        );
    }
}

#[test]
fn transcribing_produces_no_output_line() {
    // Transcribing is an internal liveness ping only — matches the
    // pre-existing behavior (never printed), not a US5 regression.
    assert!(render_event_line(&OrchestratorEvent::Transcribing).is_none());
}

#[test]
fn markers_are_unique_per_distinct_kind_of_event() {
    // FR-028: meaning must survive with colour/emoji removed — which
    // requires every *distinct* kind of line to have its own marker, not a
    // shared generic one two different meanings could collide on.
    let loading = render_event_line(&OrchestratorEvent::Loading).unwrap();
    let ready = render_event_line(&OrchestratorEvent::Ready).unwrap();
    let snippet = render_event_line(&OrchestratorEvent::Snippet("x".into())).unwrap();
    let final_ = render_event_line(&OrchestratorEvent::Final("x".into())).unwrap();
    let unstable = render_event_line(&OrchestratorEvent::Unstable("x".into())).unwrap();
    let done_empty = render_event_line(&OrchestratorEvent::Done(String::new())).unwrap();
    let done = render_event_line(&OrchestratorEvent::Done("x".into())).unwrap();
    let error = render_event_line(&OrchestratorEvent::Error {
        code: "x".into(),
        message: "x".into(),
    })
    .unwrap();
    let dropped = render_event_line(&OrchestratorEvent::AudioDropped(
        myna_orchestrator::DropReason::NotResident,
    ))
    .unwrap();

    let markers: Vec<&str> = [
        &loading, &ready, &snippet, &final_, &unstable, &done, &error, &dropped,
    ]
    .iter()
    .map(|l| marker_of(&l.text).unwrap())
    .collect();
    let mut sorted = markers.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        markers.len(),
        sorted.len(),
        "markers must be unique per distinct event kind: {markers:?}"
    );
    // The empty-transcript Done still uses the same [done] marker as a
    // populated one — same *kind* of event (session completed), so sharing
    // is correct here, not a collision with a different meaning.
    assert_eq!(marker_of(&done_empty.text), marker_of(&done.text));
}

// ── T2/T073: output is line-oriented, append-only — no write contains a
//    bare `\r`/ANSI cursor-movement sequence across a full session ────────

#[test]
fn a_full_simulated_session_never_contains_a_carriage_return_or_ansi_sequence() {
    let script = [
        OrchestratorEvent::Loading,
        OrchestratorEvent::Ready,
        OrchestratorEvent::Transcribing,
        OrchestratorEvent::Snippet("h".into()),
        OrchestratorEvent::Snippet("he".into()),
        OrchestratorEvent::Final("hello".into()),
        OrchestratorEvent::Done("hello".into()),
    ];
    for event in &script {
        if let Some(line) = render_event_line(event) {
            assert!(
                is_plain_line(&line.text),
                "line {:?} for {event:?} must be plain (no \\r / ANSI)",
                line.text
            );
        }
    }
}

#[test]
fn error_and_dropped_audio_route_to_stderr_everything_else_to_stdout() {
    let error = render_event_line(&OrchestratorEvent::Error {
        code: "x".into(),
        message: "x".into(),
    })
    .unwrap();
    assert_eq!(error.stream, OutputStream::Stderr);

    let dropped = render_event_line(&OrchestratorEvent::AudioDropped(
        myna_orchestrator::DropReason::NotResident,
    ))
    .unwrap();
    assert_eq!(dropped.stream, OutputStream::Stderr);

    let ready = render_event_line(&OrchestratorEvent::Ready).unwrap();
    assert_eq!(ready.stream, OutputStream::Stdout);
}

// ── T3/T074: a forced failure uses the exact `FailurePresentation` text
//    from the shared registry, not a separate ad-hoc CLI-only string
//    (contract F2, shares the fixture T063 established) ──────────────────
//
// Note: `myna-cli` is a `[[bin]]`-only crate (no library target), so this
// file cannot import `main.rs`'s own `render_failure` (the terminal
// `SessionOutcome::Failed`/`BackendError` outcome path, T071) to assert
// byte-for-byte equality against `render_event_line`'s mid-stream `Error`
// path here. Both call sites are `format!("[error] {}",
// presentation.render(detail))` over the identical shared
// `FailurePresentation::render` — verified by direct code inspection during
// review — so the two cannot independently diverge in wording (contract
// F2/F3) even though this specific test file can only exercise one side.

#[test]
fn a_mid_stream_error_uses_the_exact_registered_failure_presentation_text() {
    let presentation = myna_core::failure::lookup(myna_core::failure::CODE_INFERENCE_FAILED)
        .expect("CODE_INFERENCE_FAILED must be registered");

    let line = render_event_line(&OrchestratorEvent::Error {
        code: myna_core::failure::CODE_INFERENCE_FAILED.into(),
        message: "raw backend detail".into(),
    })
    .unwrap();

    assert!(line.text.contains(presentation.message));
    assert!(line.text.contains(presentation.recovery_action));
    assert!(line.text.contains("raw backend detail"));
    // Not a separate ad-hoc string: it's built from the exact same
    // `render()` call every other surface uses.
    assert_eq!(
        line.text,
        format!("[error] {}", presentation.render(Some("raw backend detail")))
    );
}

#[test]
fn an_unrecognized_error_code_falls_back_to_the_shared_unknown_failure_presentation() {
    let presentation = myna_core::failure::lookup(myna_core::failure::UNKNOWN_BACKEND_FAILURE)
        .expect("UNKNOWN_BACKEND_FAILURE must be registered");

    let line = render_event_line(&OrchestratorEvent::Error {
        code: "some_code_never_registered".into(),
        message: "boom".into(),
    })
    .unwrap();

    assert!(line.text.contains(presentation.message));
    assert!(line.text.contains("boom"));
}
