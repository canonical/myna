"""Streaming session state for progressive transcription (feature 007-streaming-mode).

This module provides the SegmentAccumulator for tracking committed and unstable
segments during a streaming session. The accumulator enforces the append-only
invariant for committed text (FR-005) and manages the revision semantics for
unstable text.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from myna.core.events import Disposition


@dataclass
class SegmentAccumulator:
    """Accumulates transcription segments during a streaming session.
    
    Committed segments are append-only (never retracted). Unstable segments
    may be revised or superseded. The accumulator tracks both for metrics
    and validation purposes.
    """
    
    committed_segments: list[str] = field(default_factory=list)
    """List of committed text segments in order (append-only)."""
    
    committed_count: int = 0
    """Count of committed segments received."""
    
    unstable_text: str | None = None
    """Current unstable hypothesis (replaced on each unstable event)."""
    
    def add_segment(self, text: str, disposition: Disposition) -> None:
        """Add a transcription segment with its disposition.
        
        Args:
            text: The transcribed text
            disposition: Whether this text is committed or unstable
        """
        if disposition == Disposition.COMMITTED:
            if text:  # Skip empty segments
                self.committed_segments.append(text)
                self.committed_count += 1
        else:  # Disposition.UNSTABLE
            # Replace the current unstable hypothesis
            self.unstable_text = text
    
    def get_transcript(self) -> str:
        """Get the current committed transcript (concatenation of all committed segments)."""
        return "".join(self.committed_segments)
    
    def clear_unstable(self) -> None:
        """Clear the unstable hypothesis (e.g., after a committed segment supersedes it)."""
        self.unstable_text = None
    
    def validate_against_final(self, final_transcript: str) -> bool:
        """Validate that the accumulated committed segments match the final transcript.
        
        This checks the append-only invariant: the committed text should be a
        prefix of (or equal to) the final transcript.
        
        Args:
            final_transcript: The complete final transcript
            
        Returns:
            True if the committed segments are consistent with the final transcript
        """
        accumulated = self.get_transcript()
        # The accumulated text should be a prefix of the final (allowing for trailing spaces)
        return final_transcript.startswith(accumulated.rstrip())
