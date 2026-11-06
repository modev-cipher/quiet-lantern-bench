"""Fetches a pinned GPU build toolchain from scripts/release/gpu-toolchain.json.

  fetch_gpu_toolchain.py cuda|vulkan|nvshmem DEST      prints `export NAME=value` lines
  fetch_gpu_toolchain.py cuda|vulkan|nvshmem DEST --github  appends to $GITHUB_ENV and $GITHUB_PATH

Every archive is checked against the size and sha256 its publisher lists (NVIDIA's
redistrib JSON, LunarG's sha endpoint) before it is unpacked; a finished toolchain is reused.
"""

import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path
from typing import Any

MANIFEST = Path(__file__).with_name("gpu-toolchain.json")
CHUNK = 1 << 20
USER_AGENT = "txtcaster-toolchain-fetch/1"
ATTEMPTS = 5


def fail(message: str) -> None:
    print(f"fetch_gpu_toolchain: {message}", file=sys.stderr)
    sys.exit(1)


def platform_key() -> str:
    if platform.machine().lower() not in ("x86_64", "amd64"):
        fail(f"no pinned GPU toolchain for {platform.machine()}")
    if sys.platform.startswith("linux"):
        return "linux-x86_64"
    if sys.platform == "win32":
        return "windows-x86_64"
    fail(f"no pinned GPU toolchain for {sys.platform}")
    raise AssertionError


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(CHUNK), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_into(url: str, partial: Path) -> None:
    have = partial.stat().st_size if partial.is_file() else 0
    headers = {"User-Agent": USER_AGENT}
    if have:
        headers["Range"] = f"bytes={have}-"
    request = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(request, timeout=120) as response:
        resumed = have > 0 and response.status == 206
        with partial.open("ab" if resumed else "wb") as out:
            shutil.copyfileobj(response, out, CHUNK)


def download(url: str, sha256: str, size: int, folder: Path) -> Path:
    folder.mkdir(parents=True, exist_ok=True)
    target = folder / url.rsplit("/", 1)[-1]
    if target.is_file() and target.stat().st_size == size and sha256_of(target) == sha256:
        return target
    partial = target.with_name(target.name + ".part")
    print(f"downloading {url} ({size / 1e6:.0f} MB)", file=sys.stderr)
    for attempt in range(1, ATTEMPTS + 1):
        if partial.is_file() and partial.stat().st_size > size:
            partial.unlink()
        if partial.is_file() and partial.stat().st_size == size:
            break
        try:
            fetch_into(url, partial)
        except OSError as error:
            if attempt == ATTEMPTS:
                fail(f"{url}: {error} after {ATTEMPTS} attempts")
            print(f"retrying {url} after: {error}", file=sys.stderr)
    if not partial.is_file() or partial.stat().st_size != size:
        partial.unlink(missing_ok=True)
        fail(f"{url}: expected {size} bytes, got another size")
    actual = sha256_of(partial)
    if actual != sha256:
        partial.unlink()
        fail(f"{url}: sha256 {actual} does not match the pinned {sha256}")
    partial.replace(target)
    return target


def unpack(archive: Path, into: Path) -> None:
    into.mkdir(parents=True, exist_ok=True)
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(into)
    else:
        with tarfile.open(archive, "r:*") as bundle:
            bundle.extractall(into, filter="data")


def single_child(folder: Path) -> Path:
    children = [child for child in folder.iterdir()]
    if len(children) != 1 or not children[0].is_dir():
        fail(f"{folder}: expected one top-level directory in the archive")
    return children[0]


def merge_into(source: Path, target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        destination = target / entry.name
        if entry.is_dir() and not entry.is_symlink() and destination.is_dir():
            merge_into(entry, destination)
        else:
            if destination.is_symlink() or destination.is_file():
                destination.unlink()
            entry.replace(destination)


def stamp_matches(root: Path, stamp: str) -> bool:
    marker = root / ".txtcaster-toolchain"
    return marker.is_file() and marker.read_text() == stamp


def write_stamp(root: Path, stamp: str) -> None:
    (root / ".txtcaster-toolchain").write_text(stamp)


def fetch_cuda(spec: dict[str, Any], key: str, dest: Path) -> tuple[dict[str, str], list[Path]]:
    archives = spec["archives"][key]
    root = dest / f"cuda-{spec['version']}"
    stamp = json.dumps(archives, sort_keys=True)
    if not stamp_matches(root, stamp):
        shutil.rmtree(root, ignore_errors=True)
        root.mkdir(parents=True)
        for entry in archives:
            archive = download(
                spec["base_url"] + entry["path"], entry["sha256"], entry["size"], dest / "downloads"
            )
            with tempfile.TemporaryDirectory(dir=dest) as staging:
                unpack(archive, Path(staging))
                merge_into(single_child(Path(staging)), root)
        write_stamp(root, stamp)
    lib64 = root / "lib64"
    if not key.startswith("windows") and not lib64.exists():
        # nvcc.profile links from $(TOP)/lib64, where the full toolkit installer puts them.
        lib64.symlink_to("lib")
    nvcc = root / "bin" / ("nvcc.exe" if key.startswith("windows") else "nvcc")
    if not nvcc.is_file():
        fail(f"{nvcc} is missing after unpacking")
    env = {"CUDA_PATH": str(root), "CUDAToolkit_ROOT": str(root), "CUDACXX": str(nvcc)}
    return env, [root / "bin"]


def fetch_vulkan(spec: dict[str, Any], key: str, dest: Path) -> tuple[dict[str, str], list[Path]]:
    entry = spec["archives"][key]
    root = dest / f"vulkan-{entry['version']}"
    stamp = json.dumps(entry, sort_keys=True)
    if key.startswith("windows"):
        sdk = root
        if not stamp_matches(root, stamp):
            installer = download(entry["url"], entry["sha256"], entry["size"], dest / "downloads")
            shutil.rmtree(root, ignore_errors=True)
            subprocess.run(
                [
                    str(installer),
                    "--root",
                    str(root),
                    "--accept-licenses",
                    "--default-answer",
                    "--confirm-command",
                    "install",
                ],
                check=True,
                stdout=sys.stderr,
            )
            write_stamp(root, stamp)
        binaries = sdk / "Bin"
    else:
        sdk = root / entry["version"] / "x86_64"
        if not stamp_matches(root, stamp):
            archive = download(entry["url"], entry["sha256"], entry["size"], dest / "downloads")
            shutil.rmtree(root, ignore_errors=True)
            unpack(archive, root)
            write_stamp(root, stamp)
        binaries = sdk / "bin"
    glslc = binaries / ("glslc.exe" if key.startswith("windows") else "glslc")
    if not glslc.is_file():
        fail(f"{glslc} is missing after installing the Vulkan SDK")
    return {"VULKAN_SDK": str(sdk)}, [binaries]


APACHE_MARKERS = ("Apache License", "Version 2.0, January 2004")
PROPRIETARY_MARKERS = ("LICENSE AGREEMENT FOR NVIDIA", "End User License Agreement")


def fetch_nvshmem(spec: dict[str, Any], key: str, dest: Path) -> tuple[dict[str, str], list[Path]]:
    if not key.startswith("linux"):
        fail("NVSHMEM ships only in the Linux NVIDIA engine")
    entry = spec["archives"][key]
    root = dest / f"nvshmem-{entry['version']}"
    stamp = json.dumps(entry, sort_keys=True)
    if not stamp_matches(root, stamp):
        archive = download(entry["url"], entry["sha256"], entry["size"], dest / "downloads")
        shutil.rmtree(root, ignore_errors=True)
        root.mkdir(parents=True)
        with tarfile.open(archive, "r:xz") as bundle:
            wanted = [
                member
                for member in bundle.getmembers()
                if "libnvshmem_host.so" in member.name.rsplit("/", 1)[-1]
                or "licen" in member.name.rsplit("/", 1)[-1].lower()
            ]
            bundle.extractall(root, members=wanted, filter="data")
        write_stamp(root, stamp)
    licences = sorted(
        (path for path in root.rglob("*") if path.is_file() and "licen" in path.name.lower()),
        key=lambda path: (len(path.relative_to(root).parts), str(path)),
    )
    if not licences:
        fail(f"{entry['url']} carries no licence file")
    text = licences[0].read_text(errors="replace")
    if not all(marker in text for marker in APACHE_MARKERS) or any(
        marker in text for marker in PROPRIETARY_MARKERS
    ):
        fail(f"{licences[0].relative_to(root)} is not the Apache-2.0 licence; refusing to bundle")
    hosts = [path for path in root.rglob("libnvshmem_host.so.3") if path.exists()]
    if len(hosts) != 1:
        fail(f"expected one libnvshmem_host.so.3 in {entry['url']}, found {len(hosts)}")
    found = licences[0].relative_to(root)
    print(f"NVSHMEM {entry['version']} licence: {found} (Apache-2.0)", file=sys.stderr)
    env = {"NVSHMEM_HOST_LIB": str(hosts[0].resolve()), "NVSHMEM_LICENSE": str(licences[0])}
    return env, []


def shell_path(path: Path) -> str:
    if sys.platform != "win32":
        return str(path)
    converted = subprocess.run(["cygpath", "-u", str(path)], capture_output=True, text=True)
    return converted.stdout.strip() if converted.returncode == 0 else str(path)


def main() -> None:
    parser = argparse.ArgumentParser(prog="fetch_gpu_toolchain")
    parser.add_argument("toolchain", choices=("cuda", "vulkan", "nvshmem"))
    parser.add_argument("dest", type=Path)
    parser.add_argument("--github", action="store_true")
    args = parser.parse_args()

    spec = json.loads(MANIFEST.read_text())[args.toolchain]
    key = platform_key()
    dest = args.dest.absolute()
    fetch = {"cuda": fetch_cuda, "vulkan": fetch_vulkan, "nvshmem": fetch_nvshmem}[args.toolchain]
    env, paths = fetch(spec, key, dest)

    if args.github:
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as out:
            for name, value in env.items():
                out.write(f"{name}={value}\n")
        with open(os.environ["GITHUB_PATH"], "a", encoding="utf-8") as out:
            for path in paths:
                out.write(f"{path}\n")
        return
    for name, value in env.items():
        print(f"export {name}={shlex.quote(value)}")
    if paths:
        joined = ":".join(shell_path(path) for path in paths)
        print(f'export PATH={shlex.quote(joined)}:"$PATH"')


if __name__ == "__main__":
    main()
