"""``myna-bench text --eval`` end to end: the real CLI parser, a real HTTP endpoint,
recorded outputs, the blind corpus slice and the report that comes out."""

from __future__ import annotations

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from myna.benchmarker import text_corpus, text_eval, text_suite


class Echo(BaseHTTPRequestHandler):
    """An OpenAI-compatible endpoint that answers every prompt with the user text plus an invented word."""

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        text = body["messages"][-1]["content"].removesuffix(" /no_think")
        reply = json.dumps({"choices": [{"message": {"content": f"<think>x</think>{text} zzz"}}]})
        self.send_response(200)
        self.send_header("Content-Length", str(len(reply)))
        self.end_headers()
        self.wfile.write(reply.encode())

    def log_message(self, *args):
        pass


@pytest.fixture
def endpoint():
    server = HTTPServer(("127.0.0.1", 0), Echo)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_port}"
    server.shutdown()
    server.server_close()


def test_eval_scores_arms_and_recorded_outputs_on_a_blind_slice(tmp_path, endpoint, capsys):
    items = [i for i in text_corpus.load_eval("run2") if (i.op, i.lang) == ("numbers", "fr")]
    recorded = tmp_path / "recorded"
    recorded.mkdir()
    oracle = ({"id": i.id, "output": i.refs[0], "ms": 1} for i in items)
    (recorded / "oracle.jsonl").write_text("".join(json.dumps(r) + "\n" for r in oracle))

    parser = argparse.ArgumentParser()
    text_suite.add_arguments(parser)
    args = parser.parse_args(
        [
            "--eval",
            "run2",
            "--op",
            "numbers",
            "--lang",
            "fr",
            "--results",
            str(recorded),
            "--arm",
            "rules=identity",
            "--arm",
            f"openai:{endpoint}#qwen3-echo",
            "--report",
            str(tmp_path / "report.md"),
        ]
    )
    text_suite.cmd_text(args)

    rows = {
        line.split("|")[1].strip(): [c.strip() for c in line.split("|")[2:]]
        for line in capsys.readouterr().out.splitlines()
        if line.startswith("| ") and "---" not in line and not line.startswith("| arm")
    }
    corpus = [i for i in items if i.kind != "clean"]
    assert f"(corpus {len(corpus)}, clean 100)" in (tmp_path / "report.md").read_text()
    assert float(rows["oracle"][0]) == 100.0 and float(rows["oracle"][4]) == 0.0  # acc, break
    assert float(rows["rules"][0]) < 50 and rows["rules"][1] == "-"  # identity changes nothing
    assert float(rows["qwen3-echo"][4]) == 100.0  # the invented word alters every clean sentence
    # The hybrid verifier rejects the model's invented words and falls back.
    assert float(rows["hyb:qwen3-echo"][4]) == 0.0
    assert set(rows) == {"oracle", "rules", "qwen3-echo", "hyb:qwen3-echo"}


def test_eval_refuses_a_slice_without_items():
    args = argparse.Namespace(
        eval_run="run2", op=["nope"], lang=None, arm=[], results=None, report=None
    )
    with pytest.raises(SystemExit, match="no item matches"):
        text_eval.cmd_eval(args)
