import json
import re
from pathlib import Path

from txtcaster_engine.llm.backend import LLMBackend, ResourcePlan
from txtcaster_engine.llm.registry import _default_factory

FAKE_GGUF_MAGIC = b"FAKEGGUF"

_NUMBERED_RE = re.compile(r"^\d+\. ", flags=re.MULTILINE)


def is_fake_model(model_path: str) -> bool:
    path = Path(model_path)
    if not path.is_file():
        return False
    with path.open("rb") as handle:
        return handle.read(len(FAKE_GGUF_MAGIC)) == FAKE_GGUF_MAGIC


def fake_aware_factory(model_path: str, plan: ResourcePlan, threads: int | None) -> LLMBackend:
    if is_fake_model(model_path):
        return FakeLLMBackend(plan)
    return _default_factory(model_path, plan, threads)


class FakeLLMBackend(LLMBackend):
    def __init__(self, plan: ResourcePlan) -> None:
        self.plan = plan
        self.calls: list[str] = []

    def _plan_echo(self) -> str:
        return (
            f"device {self.plan.device.learned_key} with {self.plan.n_gpu_layers} "
            f"of {self.plan.n_layers_total} gpu layers"
        )

    def generate(
        self,
        prompt: str,
        max_tokens: int,
        temperature: float = 0.0,
        stop: list[str] | None = None,
    ) -> str:
        self.calls.append(prompt)
        if "Summarize the lecture" in prompt:
            return f"Running summary ({self._plan_echo()})."
        if "text-to-speech" in prompt:
            count = len(_NUMBERED_RE.findall(prompt))
            return json.dumps(
                [f"Spoken paragraph number {i + 1} ({self._plan_echo()})." for i in range(count)]
            )
        return json.dumps(
            [{"text": f"A lecture paragraph explaining the source ({self._plan_echo()})."}]
        )

    def context_window(self) -> int:
        return self.plan.ctx

    def unload(self) -> None:
        pass
