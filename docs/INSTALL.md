# txtCaster — Install and Run

Moved from the repository README: link URL targets were rebased to this file's location; every other byte of the moved sections is unchanged. The former README is archived at `docs/archive/2026-09/README.md`.

## System requirements

**Operating system.** Linux or Windows, 64-bit. The instructions below were verified on
Fedora Linux 43. The Debian/Ubuntu package list is the one used by the project's CI.

**GPU is optional, not required.** txtCaster runs entirely on CPU if you have no usable GPU,
and it will detect this and adjust its recommendations. A GPU makes conversions substantially
faster. The engine's default build supports CPU and Vulkan, so most reasonably modern GPUs
can help; NVIDIA cards can additionally use a CUDA build. Models that do not fit in video
memory are split across GPU and CPU rather than being refused.

Be aware that a full CPU-only conversion of a long book, especially in Teacher mode, can take
hours. txtCaster is designed for this case: it gives you a time estimate up front, keeps
checkpoints, and can hold the machine awake to run unattended.

**Memory.** Requirements depend entirely on which models you install. The smallest useful
setup needs roughly 2 GB of free RAM; larger language and vision models ask for 6 to 20 GB.
The app tells you what each model needs before you download it.

**Disk.** Plan generously if you are building from source. The Python engine's dependencies
are several gigabytes on their own, and the native build directory grows large during a debug
build. Budget around 30 GB of free space for a source build, plus whatever models you install
and the audio you generate. An installed release package is far smaller than the build tree.


## The engine

The installer does not carry the engine. The app offers it when something first needs it, and from
Settings. Three packages exist; each downloads from a signed manifest and can be cancelled or retried:

| Package | Unpacked size | What it enables |
|---|---|---|
| Reader (`lite`) | about 90 MB on Windows, 165 MB on Linux | Importing books and reading them; narration and tutor on your own provider keys |
| CPU (`cpu`) | about 1.7 GB on Windows, 2.7 GB on Linux | Everything above plus local narration, local scripts and local timing |
| NVIDIA (`cuda13`) | about 6 GB | The same, accelerated on supported NVIDIA cards |

Without any engine the app opens, plays existing books and runs the tutor on your own provider keys.
Scanned PDFs need layout/OCR models, which the Reader package cannot run. FFmpeg ships inside every
installer and needs no setup.

### Running against a development engine

A release build never picks up Python code from the working directory. To use a checkout's engine with
any build, point at it explicitly:

```bash
TXTCASTER_DEV_ENGINE=/absolute/path/to/txtCast/engine txtcaster
```

The folder must hold `pyproject.toml` and its `.venv`. Debug builds (`tauri dev`) fall back to the checkout
they were compiled from.

## Where txtCaster keeps its files

| | Linux | Windows |
|---|---|---|
| Books, audio, engine, models, database | `~/.local/share/txtcaster` | `%LOCALAPPDATA%\\txtcaster-data` |
| Cache | `~/.cache/txtcaster` | `%LOCALAPPDATA%\\txtcaster-cache` |

Uninstalling on Windows removes the app and the cache. Books, audio, engine and models stay unless the uninstaller's "delete application data" box is ticked.

## Building Linux release packages

```bash
TXTCASTER_UPDATE_PUBKEY=<public key> scripts/release/build_linux_container.sh app
scripts/release/build_linux_container.sh engine lite
```

These build inside an Ubuntu 22.04 container (Podman), so the packages run on glibc 2.35 and newer and the
host's Python and toolchain stay untouched.
The AppImage carries WebKit, its media stack and FFmpeg. It expects these from the host, all present on
desktop distributions: `libasound`, `libEGL`, `libGLESv2`, `libfontconfig`, `libfreetype`, `libharfbuzz`,
`libfribidi`, `libwayland` and the X11 client libraries.
Its WebKit crashes on COLRv1 color fonts, so the AppImage draws emoji as plain glyphs; the deb and rpm use the
system WebKit and keep color emoji.
All three carry the PulseAudio 17.0 client library, because releases 15.99 to 16.1 can stall playback after a
seek. The AppImage always uses it; the deb and rpm use it only where the system library is one of those
releases (Ubuntu 22.04, Ubuntu 24.04 and Debian 12 among the tested images), by restarting themselves once at launch with
`/usr/lib/txtCaster/pulse` first on `LD_LIBRARY_PATH`.

On a Linux session where `xdg-desktop-portal` is installed but cannot start, every GTK application waits about
25 seconds at launch for the session bus to give up on it. txtCaster then opens normally.

## Models and disk space

The application itself ships without model weights. On first launch you pick what to install.
Only one thing is strictly required.

| Purpose | Required | Example | Download size |
|---|---|---|---|
| Narration voice | Yes | Compact narration voice (English) | 0.06 GB |
| Narration voice | Yes | Standard narration voice | 0.36 GB |
| Teacher mode | Only for Teacher mode | Compact teaching language model | 2.49 GB |
| Figure descriptions | Optional | Compact vision model | 3.33 GB |

Larger and higher-quality options are available in the catalog for all three categories, up
to roughly 17 GB for the largest language model. You can change the model on a per-job basis;
the global defaults only prefill the form.

A minimal working installation is therefore well under half a gigabyte of models. Teacher
mode and figure descriptions are the features that cost real disk space.


## Install and run

There are no published binaries yet, so the application is built from source. The full
recipe — system packages, toolchains, first run, and packaging an installer — lives in
[`CONTRIBUTING.md`](../CONTRIBUTING.md). Expect the first build to take a while and to need
around 30 GB of disk; later launches are fast.


## Development

This README covers using the application. For architecture, specifications, the task
breakdown and contribution workflow, start with [`AGENTS.md`](../AGENTS.md) and the
documentation package in [`docs/`](README.md).

