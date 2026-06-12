"""Ubuntu Desktop dictation client (UD129) — interface stubs.

These are contracts only; implementation is a later workstream. They exist
now so that ideation about the desktop side happens against the same shared
vocabulary (``myna.core``) the testbed already exercises.
"""

from myna.desktop.controller import DictationState
from myna.desktop.textout import TextInjector

__all__ = ["DictationState", "TextInjector"]
