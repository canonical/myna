"""Command-line entry point for the UbuSTT server.

    myna-server --socket /path/to/ubustt.sock --model tiny
    python -m myna.server --socket ... --model base --preload

Serves transcription sessions over WebSocket on the given Unix socket until
SIGTERM/SIGINT. Model weights load lazily on the first session by default;
``--preload`` loads (and warms) at startup instead, trading idle memory for
first-session latency — the trade-off IE114 leaves to configuration.

Privacy: transcription content is never logged.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
import os
import signal
import sys
from pathlib import Path

log = logging.getLogger("myna.server")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="myna-server", description=__doc__)
    parser.add_argument("--socket", required=True, type=Path, help="Unix socket path to serve on")
    parser.add_argument(
        "--adapter",
        default="whisper",
        choices=("whisper", "nemotron", "qwen-c", "parakeet", "sherpa", "funasr", "audio8", "fake"),
        help="ASR backend ('fake' = scripted, no model — for wire/contract testing)",
    )
    parser.add_argument(
        "--model",
        default=None,
        help="model id/path; default per adapter (whisper: tiny, nemotron: streaming "
        "FastConformer; sherpa/funasr: ONNX model dir — default: staged cache snapshot)",
    )
    parser.add_argument(
        "--device",
        default=None,
        choices=("cpu", "cuda"),
        help="inference device; default per adapter (whisper: cpu, nemotron: cuda)",
    )
    parser.add_argument(
        "--compute-type", default="default", help="CTranslate2 compute type (whisper)"
    )
    parser.add_argument(
        "--att-context-size", default=None, help="Nemotron latency dial, e.g. '70,0'"
    )
    parser.add_argument(
        "--socket-mode",
        default="0666",
        help="octal permission bits for the socket file (default world-connectable; "
        "tighten via dir permissions or dedicated group in production)",
    )
    parser.add_argument("--preload", action="store_true", help="load the model at startup")
    parser.add_argument(
        "--streaming",
        action="store_true",
        help="enable streaming mode (progressive committed segments; whisper/nemotron/parakeet)",
    )
    parser.add_argument(
        "--stream-cadence-s",
        type=float,
        default=None,
        help="seconds of new audio between re-decode ticks (whisper streaming)",
    )
    parser.add_argument(
        "--stream-window-cap-s",
        type=float,
        default=None,
        help="max uncommitted audio window in seconds before force-commit (whisper streaming)",
    )
    parser.add_argument(
        "--stream-beam-size",
        type=int,
        default=None,
        help="beam size for streaming re-decode ticks (default 1 = greedy; "
        "5 matches batch decode quality at ~5x tick cost)",
    )
    parser.add_argument(
        "--stream-arm-s",
        type=float,
        default=None,
        help="seconds of uncommitted audio before Parakeet SilenceCut can commit "
        "(default 15; lower for earlier chunks)",
    )
    parser.add_argument(
        "--stream-silence-cut-s",
        type=float,
        default=None,
        help="pause duration in seconds that cuts a Parakeet streaming chunk (default 0.5)",
    )
    parser.add_argument(
        "--stream-force-cut-s",
        type=float,
        default=None,
        help="max uncommitted audio window before Parakeet force-commits (default 60)",
    )
    parser.add_argument(
        "--stream-partial-cadence-s",
        type=float,
        default=None,
        help="seconds between Parakeet unstable-partial ticks between commits "
        "(default 0.5; 0 shows nothing until the first cut)",
    )
    parser.add_argument(
        "--stream-partial-tail-s",
        type=float,
        default=None,
        help="cap each Parakeet partial tick at the last N seconds of the window "
        "(default 0 = whole window; capping shows only those N seconds)",
    )
    parser.add_argument(
        "--sleep-idle-seconds",
        type=float,
        default=0.0,
        help="release the model after N idle seconds (0 = never)",
    )
    parser.add_argument(
        "--idle-action",
        default="unload",
        choices=("unload", "exit"),
        help="on idle: 'unload' (drop weights, keep serving) or 'exit' (for socket activation)",
    )
    parser.add_argument(
        "--funasr-language",
        default="auto",
        choices=("auto", "zh", "en", "yue", "ja", "ko"),
        help="language for SenseVoice decoding (default: auto)",
    )
    parser.add_argument(
        "--funasr-textnorm",
        default="woitn",
        choices=("woitn", "withitn"),
        help="ITN mode: woitn (readable text, default), withitn (digits/dates)",
    )
    parser.add_argument(
        "--audio8-language",
        default="auto",
        choices=("auto",),
        help="Audio8 language selection (auto only — pinning is inert; "
        "recognition covers en/zh/fr/de/ja/ko/yue)",
    )
    parser.add_argument(
        "--audio8-precision",
        default="int8",
        choices=("int8", "int4"),
        help="Audio8 decoder precision (default: int8)",
    )
    parser.add_argument(
        "--audio8-max-new-tokens",
        type=int,
        default=256,
        help="Audio8 generation cap (bounded decode, default: 256)",
    )
    return parser


def _default_if_none(value: float | None, fallback: float) -> float:
    """`x or fallback` is wrong wherever 0 is a meaningful setting."""
    return fallback if value is None else value


def build_adapter(args: argparse.Namespace):
    """Construct the chosen adapter, importing its extra lazily so --help and
    the other adapter work without it installed."""
    if args.adapter == "fake":
        # No model, no extra: the permanent regression fixture, served over the
        # wire so transport/contract clients (e.g. the Rust orchestrator) can
        # integration-test without a GPU or model. See myna.testbed.fake.
        from myna.testbed.fake import FakeAdapter

        return FakeAdapter()

    if args.adapter == "whisper":
        from myna.testbed.whisper import FasterWhisperAdapter

        return FasterWhisperAdapter(
            args.model or "tiny",
            device=args.device or "cpu",
            compute_type=args.compute_type,
            streaming=args.streaming,
            # getattr: programmatic callers (tests, embedding) may build the
            # namespace without the streaming flags.
            stream_cadence_s=getattr(args, "stream_cadence_s", None) or 1.0,
            stream_window_cap_s=getattr(args, "stream_window_cap_s", None) or 30.0,
            stream_beam_size=getattr(args, "stream_beam_size", None) or 1,
        )

    if args.adapter == "parakeet":
        from myna.testbed.parakeet import ParakeetAdapter

        return ParakeetAdapter(
            args.model,
            streaming=args.streaming,
            stream_arm_s=getattr(args, "stream_arm_s", None) or 15.0,
            stream_silence_cut_s=getattr(args, "stream_silence_cut_s", None) or 0.5,
            stream_force_cut_s=getattr(args, "stream_force_cut_s", None) or 60.0,
            # `or` would swallow an explicit 0, and 0 is how partials are
            # turned off.
            stream_partial_cadence_s=_default_if_none(
                getattr(args, "stream_partial_cadence_s", None), 0.5
            ),
            stream_partial_tail_s=_default_if_none(
                getattr(args, "stream_partial_tail_s", None), 0.0
            ),
        )

    if args.adapter == "sherpa":
        from myna.testbed.sherpa import SherpaAdapter

        return SherpaAdapter(args.model, streaming=args.streaming)

    if args.adapter == "funasr":
        from myna.testbed.funasr import FunasrAdapter

        # --model is the ONNX model dir, like sherpa; absent = the staged
        # ModelScope cache snapshot (dev/fetch_funasr_model.py).
        return FunasrAdapter(
            args.model,
            language=args.funasr_language,
            textnorm=args.funasr_textnorm,
        )

    if args.adapter == "audio8":
        # --model is the staged runtime dir (engine source + model_bundle);
        # absent = the HF cache snapshot (dev/fetch_audio8_model.py).
        from myna.testbed.audio8 import Audio8Adapter

        return Audio8Adapter(
            args.model,
            language=args.audio8_language,
            cache_precision=args.audio8_precision,
            max_new_tokens=args.audio8_max_new_tokens,
            device=args.device or "cpu",
        )

    if args.adapter == "qwen-c":
        # The C runtime needs a local model directory (no downloading); the
        # shared library is located via QWEN_ASR_LIB (set by the snap runtime).
        from myna.testbed.qwen import QwenCAdapter

        if not args.model:
            raise SystemExit("--adapter qwen-c requires --model <model dir>")
        return QwenCAdapter(args.model)

    from myna.testbed.nemotron import (
        DEFAULT_MODEL,
        NemotronAdapter,
        _parse_att_context_size,
    )

    return NemotronAdapter(
        args.model or DEFAULT_MODEL,
        device=args.device or "cuda",
        att_context_size=_parse_att_context_size(args.att_context_size),
        streaming=args.streaming,
    )


async def serve(args: argparse.Namespace) -> None:
    # Imported here so --help works without the adapter's extra installed.
    try:
        adapter = build_adapter(args)
    except ModuleNotFoundError as exc:
        raise SystemExit(
            f"missing dependency ({exc.name}) — install myna with the '{args.adapter}' extra"
        ) from exc

    from myna.core import serve_unix, systemd_socket
    from myna.server.lifecycle import LifecycleService, idle_monitor

    if args.preload:
        log.info("preloading adapter=%s model=%s", args.adapter, args.model or "(default)")
        await adapter._load_model()

    lifecycle = LifecycleService(adapter)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)

    monitor = None
    if args.sleep_idle_seconds > 0:
        monitor = asyncio.ensure_future(
            idle_monitor(lifecycle, args.sleep_idle_seconds, args.idle_action, stop, log=log)
        )

    # Under socket activation systemd owns the socket and hands it to us; we
    # don't bind/chmod/unlink the path (it manages those).
    inherited = systemd_socket()
    if inherited is None:
        args.socket.parent.mkdir(parents=True, exist_ok=True)
        with contextlib.suppress(FileNotFoundError):  # stale socket → bind() fails
            args.socket.unlink()

    async with serve_unix(lifecycle, args.socket, sock=inherited):
        if inherited is None:
            os.chmod(args.socket, int(args.socket_mode, 8))
        log.info(
            "serving adapter=%s candidate=%s on %s (pid %d; idle=%s/%s; activation=%s)",
            args.adapter,
            lifecycle.candidate.id,
            "systemd-socket" if inherited is not None else args.socket,
            os.getpid(),
            args.sleep_idle_seconds or "off",
            args.idle_action,
            "yes" if inherited is not None else "no",
        )
        await stop.wait()

    log.info("shutting down")
    if monitor is not None:
        monitor.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await monitor
    if inherited is None:
        with contextlib.suppress(FileNotFoundError):
            args.socket.unlink()


def _validate_streaming_args(args: argparse.Namespace) -> None:
    """Streaming tuning flags are whisper-only (contracts/strategy-config.md);
    with --streaming off they are inert (batch degenerate) — warn, don't fail."""
    tuning = [
        f"--{name}"
        for name, value in (
            ("stream-cadence-s", args.stream_cadence_s),
            ("stream-window-cap-s", args.stream_window_cap_s),
            ("stream-beam-size", args.stream_beam_size),
        )
        if value is not None
    ]
    if not args.streaming:
        if tuning:
            log.warning("%s ignored without --streaming (batch mode)", ", ".join(tuning))
    elif tuning and args.adapter != "whisper":
        # Parakeet's chunked commit has its own constants (SilenceCut); the
        # re-decode tuning flags are whisper-only (contracts/strategy-config.md).
        log.warning(
            "%s are whisper-only, ignored for --adapter %s",
            ", ".join(tuning),
            args.adapter,
        )


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    args = build_parser().parse_args(argv)
    _validate_streaming_args(args)
    try:
        asyncio.run(serve(args))
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
