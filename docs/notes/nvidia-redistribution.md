# NVIDIA redistribution in the NVIDIA build of the full engine (2026-11-03)

The `cuda13` engine variant carries NVIDIA runtime libraries so that local AI models,
voices and layout run on NVIDIA GPUs. The standard (`cpu`, Vulkan) and reader (`lite`)
engines carry none. This note records what ships, under which licence text, how each
distribution condition is met, and what still needs an owner decision.

## 1. What ships, and the licence that allows it

| Component | Platforms | Licence basis | Evidence |
| --- | --- | --- | --- |
| CUDA Runtime, cuBLAS/cuBLASLt, cuFFT/cuFFTW, cuRAND, cuSOLVER, cuSPARSE, NVRTC, nvJitLink, CUPTI, NVTX | Windows (torch/lib), Linux (nvidia-* wheels) | CUDA Toolkit EULA §1.1.1(iii) + Attachment A | CUDA 13.0.3 redist `LICENSE` (`engine/licenses/nvidia/CUDA-13.0-EULA.txt`); current EULA v13.4 at https://docs.nvidia.com/cuda/eula/index.html, read 2026-11-03 |
| cuFile | Linux | Attachment A, "NVIDIA CUDA File IO Libraries" | same |
| cuDNN 9 | Windows, Linux | NVIDIA SDK licence shipped in nvidia-cudnn-cu13 9.20.0.48 | `engine/licenses/nvidia/cuDNN-9-License.txt` |
| cuSPARSELt 0.8.1 | Linux only (the Windows torch 2.13.0+cu130 wheel has none) | its licence lists the Linux shared library | `nvidia/cusparselt/LICENSE.txt` in the wheel |
| NCCL 2.29.7 | Linux | BSD-3-Clause (package metadata says "Proprietary"; the shipped text is BSD) | wheel `License.txt` |
| NVSHMEM 3.8.0, host library only | Linux only | Apache-2.0 | `libnvshmem_host.so.3` from NVIDIA's redist archive `libnvshmem-linux-x86_64-3.8.0_cuda13-archive.tar.xz` (sha256 254f6df1…e6ae, from `redistrib_3.8.0.json`). `fetch_gpu_toolchain.py nvshmem` refuses to bundle unless the archive's own top-level licence is Apache-2.0. The PyPI wheels (3.4.5 and 3.8.0 alike) declare `LicenseRef-NVIDIA-Proprietary`, so the wheel copy torch pins is replaced |

`libtorch_cuda.so` links `libtorch_nvshmem.so`, which links `libnvshmem_host.so.3`, and links
`libcusparseLt.so.0` directly: with either library hidden, `import torch` fails. Torch 2.13.0 pins
NVSHMEM 3.4.5; its 20 imported NVSHMEM symbols are all exported by 3.8.0, and torch loaded 3.8.0
and ran CUDA matmul and convolution on an RTX 3050 (2026-11-06). The 12 bootstrap and transport
plugins (MPI, PMI, UCX, InfiniBand, libfabric) serve multi-node jobs; torch ran with all of them
hidden, so the build drops them.

### Excluded because they are outside Attachment A

`nvperf_host`, `nvperf_target`, `pcsamplingutil` (profiler metrics) and `cusolverMg`
(multi-GPU cuSOLVER). Nothing the engine loads links them: on Linux no bundled library
lists them as NEEDED. `txtcaster-engine.spec` drops them (`UNLISTED_NVIDIA`). On Windows
the frozen `--check-imports` probe imports torch, so a wrongly dropped DLL fails the build.

The GPU drivers (`libcuda`, `nvcuda.dll`, NVML, the Vulkan loader) are never bundled.

## 2. Distribution conditions (CUDA EULA §1.1.2, cuDNN §1.2)

| Condition | How it is met |
| --- | --- |
| (1) material additional functionality | txtCaster is a document-to-audio application; the libraries are runtime dependencies |
| (2) accessed only by our application | the libraries live inside the engine onedir and load only into the engine process |
| (3) sample-source notice | no NVIDIA sample source is distributed |
| (4) developer tools internal only | no compiler, profiler UI or toolkit tool ships; nvcc and the toolkit are build-only |
| (5) consistent distribution terms; protect users | **open: needs the end-user terms below** |
| (6) notify NVIDIA of known non-compliance; enforce our terms | owner obligation |

The licence texts ship inside the NVIDIA build under `licenses/nvidia/`, and `NOTICE`
names the components.

## 3. Owner decisions

1. **NVSHMEM (decided 2026-11-06):** bundle the 3.8.0 host library from NVIDIA's Apache-2.0
   redist archive, verified at build time; no install-time download.
2. **End-user terms.** txtCaster has no end-user licence for the binaries today: `LICENSE`
   is a source notice and the site terms cover the website. Condition (5) needs terms
   that protect NVIDIA's rights at least as much as the agreement. Draft in §4; it could
   be shown by the installer, published beside the downloads, or both.

## 4. Draft clause for the txtCaster end-user terms

> **Third-party components.** txtCaster includes software from third parties, listed in
> the NOTICE file that ships with it. Some optional downloads, including the NVIDIA build
> of the txtCaster engine, contain NVIDIA software licensed to Torkiv Labs by NVIDIA
> Corporation. You may use that software only as part of txtCaster and only on a
> computer with an NVIDIA GPU. You may not copy, extract, redistribute, sublicense,
> modify, reverse engineer, decompile or disassemble it, or use it apart from txtCaster.
> NVIDIA and its licensors own that software and keep all rights in it, and it is
> provided without any warranty from NVIDIA. Your use of it is also subject to the
> licence texts in the `licenses/nvidia` folder of that download. NVIDIA is not liable to
> you under these terms.

## 5. Draft email to NVIDIA (owner sends; not sent)

To: nvidia-compute-license-questions@nvidia.com

Subject: Redistribution of CUDA 13 runtime libraries, cuDNN and cuSPARSELt in a desktop application

> Hello,
>
> Torkiv Labs makes txtCaster, a desktop application for Windows and Linux that turns
> documents into narrated audio and runs its AI models locally. Users with an NVIDIA GPU
> can download an optional engine that includes NVIDIA runtime libraries, unmodified and
> in object code, used only by txtCaster. We obtain them through the PyTorch 2.13.0
> CUDA 13.0 wheels from PyPI and download.pytorch.org.
>
> We redistribute:
> - CUDA Runtime, cuBLAS, cuFFT, cuRAND, cuSOLVER, cuSPARSE, NVRTC, nvJitLink, CUPTI,
>   NVTX and cuFile, under the CUDA Toolkit EULA Attachment A;
> - cuDNN 9.20 under its SDK licence;
> - on Linux, cuSPARSELt 0.8.1.
>
> We exclude files not listed in Attachment A (nvperf_host, nvperf_target,
> pcsamplingutil, cusolverMg) and never bundle the driver. We ship the licence texts
> with the engine, and our end-user terms carry NVIDIA's restrictions.
>
> We would be grateful for confirmation that our approach, as described above, is
> consistent with your redistribution terms.
>
> Thank you,
> [name], Torkiv Labs
