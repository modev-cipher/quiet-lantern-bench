import argparse
import contextlib
import logging
import os
import socket
import sys
import threading

import uvicorn

from txtcaster_engine.logctx import JobIdFilter
from txtcaster_engine.offline import enable_offline_defaults
from txtcaster_engine.selftest import check_imports
from txtcaster_engine.server import create_app

HANDSHAKE_PREFIX = "TXTCASTER_READY"


def watch_parent_stdin(parent: int) -> None:
    with contextlib.suppress(OSError):
        while os.read(parent, 4096):
            pass
    os._exit(0)


def start_parent_watchdog() -> None:
    parent = os.dup(0)
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    threading.Thread(target=watch_parent_stdin, args=(parent,), daemon=True).start()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="txtcaster-engine")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--token", default=None)
    parser.add_argument("--watch-parent", action="store_true")
    parser.add_argument("--allow-fake-tts", action="store_true")
    parser.add_argument("--allow-fake-llm", action="store_true")
    parser.add_argument("--allow-fake-align", action="store_true")
    parser.add_argument("--check-imports", action="store_true")
    return parser.parse_args(argv)


def bind_socket(port: int) -> socket.socket:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", port))
    return sock


LOG_LEVELS = {
    "CRITICAL": logging.CRITICAL,
    "ERROR": logging.ERROR,
    "WARNING": logging.WARNING,
    "INFO": logging.INFO,
    "DEBUG": logging.DEBUG,
}
DEFAULT_LOG_LEVEL = "INFO"


def configure_logging() -> None:
    requested = os.environ.get("TXTCASTER_LOG_LEVEL", "").strip().upper() or DEFAULT_LOG_LEVEL
    level = LOG_LEVELS.get(requested)
    logging.basicConfig(
        level=level if level is not None else LOG_LEVELS[DEFAULT_LOG_LEVEL],
        format="%(levelname)s %(name)s: %(message)s job=%(job)s",
        stream=sys.stderr,
    )
    for handler in logging.getLogger().handlers:
        handler.addFilter(JobIdFilter())
    if level is None:
        logging.getLogger(__name__).warning(
            "ignoring TXTCASTER_LOG_LEVEL=%r; expected one of %s",
            requested,
            ", ".join(LOG_LEVELS),
        )


def main(argv: list[str] | None = None) -> None:
    enable_offline_defaults()
    args = parse_args(argv if argv is not None else sys.argv[1:])
    configure_logging()
    if args.check_imports:
        sys.exit(check_imports(sys.stdout))
    if args.watch_parent:
        start_parent_watchdog()
    sock = bind_socket(args.port)
    port = sock.getsockname()[1]
    print(f"{HANDSHAKE_PREFIX} {port}", flush=True)

    token = args.token or os.environ.get("TXTCASTER_TOKEN")
    if not token:
        sys.exit("error: no auth token provided (pass --token or set TXTCASTER_TOKEN)")

    app = create_app(
        token=token,
        allow_fake_tts=args.allow_fake_tts,
        allow_fake_llm=args.allow_fake_llm,
        allow_fake_align=args.allow_fake_align,
    )
    config = uvicorn.Config(app, log_level="warning")
    server = uvicorn.Server(config)
    server.run(sockets=[sock])


if __name__ == "__main__":
    main()
