import time
from dataclasses import dataclass

from txtcaster_engine.llm.backend import LLMBackend, LLMBackendError, ResourcePlan
from txtcaster_engine.llm.registry import BackendFactory, _default_factory

PROBE_PROMPT = "Count from one to twenty in words, separated by commas."
PROBE_MAX_TOKENS = 64


@dataclass(frozen=True)
class ProbeResult:
    device: str
    tok_s: float
    tokens: int
    elapsed_s: float


def probe_tok_s(
    model_path: str,
    plan: ResourcePlan,
    threads: int | None = None,
    max_tokens: int = PROBE_MAX_TOKENS,
    factory: BackendFactory = _default_factory,
) -> ProbeResult:
    if max_tokens < 1:
        raise LLMBackendError("ERR_PROBE", "max_tokens must be >= 1")
    backend: LLMBackend = factory(model_path, plan, threads)
    try:
        started = time.perf_counter()
        text = backend.generate(PROBE_PROMPT, max_tokens=max_tokens, temperature=0.0)
        elapsed = time.perf_counter() - started
    finally:
        backend.unload()
    tokens = _estimate_tokens(text, max_tokens)
    tok_s = tokens / elapsed if elapsed > 0 else 0.0
    device = plan.device.learned_key if plan.device.is_gpu and plan.n_gpu_layers > 0 else "cpu"
    return ProbeResult(device=device, tok_s=tok_s, tokens=tokens, elapsed_s=elapsed)


def _estimate_tokens(text: str, max_tokens: int) -> int:
    words = len(text.split())
    return max(1, min(max_tokens, words))
