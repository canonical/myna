#!/usr/bin/env python3
"""Present a TCP-only OpenAI-compatible server as a text share, for testing.

Myna's daemon has no network access, so it reaches a chat model only over
a Unix socket. Until such snaps serve one (canonical/inference#84), this
relays bytes between a socket and the server's loopback port, in the
layout a connected share has:

    SHARE_DIR/SNAP_NAME/{provider.env,chat.sock}

Run the daemon with MYNA_TEXT_SHARES=SHARE_DIR to find it. Development
only: it is never packaged.

    dev/openai-uds-shim.py --port 8336 --share-dir $XDG_RUNTIME_DIR/myna-text-share
"""

import argparse
import asyncio
import os
from pathlib import Path


async def pipe(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    finally:
        writer.close()


async def relay(port: int, client_r: asyncio.StreamReader, client_w: asyncio.StreamWriter) -> None:
    try:
        server_r, server_w = await asyncio.open_connection("127.0.0.1", port)
    except OSError:
        client_w.close()
        return
    await asyncio.gather(
        pipe(client_r, server_w), pipe(server_r, client_w), return_exceptions=True
    )


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--port", type=int, required=True, help="the server's loopback port")
    ap.add_argument("--name", default="gemma4-shim", help="SNAP_NAME of the share")
    ap.add_argument(
        "--share-dir",
        type=Path,
        default=Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp")) / "myna-text-share",
    )
    args = ap.parse_args()

    share = args.share_dir / args.name
    share.mkdir(mode=0o700, parents=True, exist_ok=True)
    (share / "provider.env").write_text(f"SNAP_NAME={args.name}\nUNIX_SOCKET=chat.sock\n")
    sock = share / "chat.sock"
    sock.unlink(missing_ok=True)
    server = await asyncio.start_unix_server(
        lambda r, w: relay(args.port, r, w), path=str(sock)
    )
    sock.chmod(0o600)
    print(f"relaying {sock} to 127.0.0.1:{args.port}")
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
