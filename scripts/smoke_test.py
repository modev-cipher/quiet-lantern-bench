import argparse
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from typing import IO

HANDSHAKE_PREFIX = "TXTCASTER_READY"
HANDSHAKE_TIMEOUT = 30.0
IMPORT_CHECK_TIMEOUT = 300.0
OFFLINE_ENV_VARS = ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE")
TOKEN = "smoke-test-token"


def fail(message: str) -> None:
    print(f"SMOKE FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read_lines(stream: IO[str], lines: "queue.Queue[str]") -> None:
    for line in iter(stream.readline, ""):
        lines.put(line)


def read_handshake(process: subprocess.Popen[str]) -> int:
    deadline = time.monotonic() + HANDSHAKE_TIMEOUT
    assert process.stdout is not None
    lines: queue.Queue[str] = queue.Queue()
    threading.Thread(target=read_lines, args=(process.stdout, lines), daemon=True).start()
    while time.monotonic() < deadline:
        if process.poll() is not None:
            fail(f"engine exited early with code {process.returncode}")
        try:
            line = lines.get(timeout=0.2)
        except queue.Empty:
            continue
        parts = line.strip().split()
        if len(parts) == 2 and parts[0] == HANDSHAKE_PREFIX:
            return int(parts[1])
    fail("handshake not received within timeout")
    raise AssertionError


def get_status(url: str, token: str | None) -> int:
    request = urllib.request.Request(url)
    if token is not None:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return int(response.status)
    except urllib.error.HTTPError as error:
        return error.code


def wait_until_listening(url: str, token: str) -> int:
    deadline = time.monotonic() + HANDSHAKE_TIMEOUT
    while time.monotonic() < deadline:
        try:
            return get_status(url, token)
        except (urllib.error.URLError, ConnectionError):
            time.sleep(0.2)
    fail("server did not start listening within timeout")
    raise AssertionError


def networkless_env() -> dict[str, str]:
    env = dict(os.environ)
    for key in OFFLINE_ENV_VARS:
        env.pop(key, None)
    return env


def check_imports(binary: str) -> None:
    result = subprocess.run(
        [binary, "--port", "0", "--token", TOKEN, "--check-imports"],
        capture_output=True,
        text=True,
        timeout=IMPORT_CHECK_TIMEOUT,
        env=networkless_env(),
    )
    print(result.stdout.strip())
    if result.returncode != 0:
        print(result.stderr.strip(), file=sys.stderr)
        fail(f"packaged engine failed its import probe (exit {result.returncode})")


def one_page_pdf() -> bytes:
    stream = b"BT /F1 24 Tf 72 720 Td (Reader engine smoke test page.) Tj ET"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R"
        b" /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    pdf = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf += b"%d 0 obj\n%s\nendobj\n" % (number, body)
    xref = len(pdf)
    pdf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for offset in offsets:
        pdf += b"%010d 00000 n \n" % offset
    pdf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1,
        xref,
    )
    return bytes(pdf)


def check_ingest(port: int) -> None:
    with tempfile.TemporaryDirectory() as folder:
        book = os.path.join(folder, "smoke.pdf")
        with open(book, "wb") as out:
            out.write(one_page_pdf())
        request = urllib.request.Request(
            f"http://127.0.0.1:{port}/ingest",
            data=json.dumps({"book_path": book, "book_id": "smoke", "layout": False}).encode(),
            headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read().decode()
    if "event: manifest" not in body:
        fail(f"/ingest produced no manifest: {body[-400:]}")
    print("/ingest: manifest")


def main() -> None:
    parser = argparse.ArgumentParser(prog="smoke_test")
    parser.add_argument("binary", help="path to the packaged txtcaster-engine binary")
    parser.add_argument(
        "--reader-only",
        action="store_true",
        help="the lite engine: no local AI imports to probe; ingest a document instead",
    )
    args = parser.parse_args()

    if not args.reader_only:
        check_imports(args.binary)

    process = subprocess.Popen(
        [args.binary, "--port", "0", "--token", TOKEN],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        port = read_handshake(process)
        print(f"handshake ok on port {port}")

        url = f"http://127.0.0.1:{port}/health"
        status = wait_until_listening(url, TOKEN)
        if status != 200:
            fail(f"/health with token returned {status}, expected 200")
        print("/health with token: 200")

        status = get_status(url, None)
        if status != 401:
            fail(f"/health without token returned {status}, expected 401")
        print("/health without token: 401")

        if args.reader_only:
            check_ingest(port)
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()

    print("SMOKE PASS")


if __name__ == "__main__":
    main()
