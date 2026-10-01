#!/usr/bin/python3
"""Print which top-level windows are ACTIVE on the real session and the text
of every editable text widget in apps whose name contains argv[1]."""
import sys
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

want = sys.argv[1] if len(sys.argv) > 1 else "text-editor"
d = Atspi.get_desktop(0)
def walk(n, depth=0):
    yield n
    if depth > 30:
        return
    try:
        c = n.get_child_count()
    except Exception:
        return
    for i in range(c):
        ch = n.get_child_at_index(i)
        if ch is not None:
            yield from walk(ch, depth + 1)
for i in range(d.get_child_count()):
    a = d.get_child_at_index(i)
    if a is None:
        continue
    name = a.get_name() or ""
    for j in range(a.get_child_count()):
        w = a.get_child_at_index(j)
        if w and w.get_state_set().contains(Atspi.StateType.ACTIVE):
            print(f"active: {name} / {w.get_name()}")
    if want in name.lower():
        for n in walk(a):
            try:
                if n.get_state_set().contains(Atspi.StateType.EDITABLE):
                    t = n.get_text_iface()
                    if t:
                        print(f"text[{n.get_role_name()}]: {t.get_text(0, t.get_character_count())!r}")
            except Exception:
                pass
