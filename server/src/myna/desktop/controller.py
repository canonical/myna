"""Dictation session controller (UD129 Speech Orchestrator) — state model.

The controller owns the session lifecycle: hotkey press opens a session
(capture target, start audio, open ``SttSession``); ``transcription.final``
events flow through post-processing into ``TextInjector.commit``; hotkey
release finishes audio and waits for ``transcription.done``; everything is
torn down afterwards and the in-memory audio buffer is discarded.

Only the state vocabulary is pinned down here. The controller implementation
is a later workstream (see docs/project-plan.md, workstream D).
"""

from __future__ import annotations

import enum


class DictationState(enum.Enum):
    """Session states, matching UD129's State Management component."""

    IDLE = "idle"
    # Hotkey pressed: acquiring target, mic, and inference session.
    STARTING = "starting"
    # Capturing and streaming audio; events flowing.
    LISTENING = "listening"
    # Hotkey released: audio finished, waiting for terminal event.
    FINALIZING = "finalizing"
    # Terminal error received or a component failed; user feedback owed.
    ERROR = "error"


# Allowed transitions; anything else is a controller bug.
TRANSITIONS: dict[DictationState, frozenset[DictationState]] = {
    DictationState.IDLE: frozenset({DictationState.STARTING}),
    DictationState.STARTING: frozenset(
        {DictationState.LISTENING, DictationState.ERROR, DictationState.IDLE}
    ),
    DictationState.LISTENING: frozenset(
        {DictationState.FINALIZING, DictationState.ERROR, DictationState.IDLE}
    ),
    DictationState.FINALIZING: frozenset({DictationState.IDLE, DictationState.ERROR}),
    DictationState.ERROR: frozenset({DictationState.IDLE}),
}
