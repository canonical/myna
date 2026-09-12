"""Vulture allow-list (feature 006, FR-006): names vulture cannot see as used.

Kept as data, not config, so each entry carries its reason. Consumed by
`vulture ... dev/vulture_allowlist.py` (vulture treats names referenced here
as used). Add an entry only with a comment stating the dynamic mechanism
(adapter-by-name loader, pytest fixture injection, protocol shim, ...).
"""

# tests/test_server.py::tiny_model_cached — pytest fixture requested by name
# by test_server_subprocess_end_to_end; its only job is to force the skip
# check, the value is never read.
# Each name below is *read* here so vulture counts it as used; the comment
# records why the dynamic use is invisible to static analysis.

# tests/test_server.py:37 — pytest fixture injected by name into
# test_server_subprocess_end_to_end; requesting the fixture *is* the use
# (it forces the whisper-model availability skip check).
tiny_model_cached  # noqa: F821,B018  # vulture whitelist: name lives in server tests
