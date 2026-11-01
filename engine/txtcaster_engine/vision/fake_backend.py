import json
from collections.abc import Callable

from txtcaster_engine.llm.backend import LLMBackendError, ResourcePlan
from txtcaster_engine.llm.fake_backend import is_fake_model
from txtcaster_engine.vision.backend import LlamaVisionBackend, VisionBackend


class FakeVisionBackend(VisionBackend):
    def __init__(self, plan: ResourcePlan) -> None:
        self.plan = plan
        self.calls: list[str] = []

    def describe(
        self,
        image_png: bytes,
        prompt: str,
        max_tokens: int,
        timeout_s: float | None = None,
        is_cancelled: Callable[[], bool] | None = None,
    ) -> str:
        self.calls.append(prompt)
        if b"OOMTRIP" in image_png:
            raise LLMBackendError("ERR_OOM", "fake out of memory")
        if b"TIMEOUT" in image_png:
            raise LLMBackendError("ERR_VISION_TIMEOUT", "fake budget exceeded")
        if b"MIDCANCEL" in image_png:
            raise LLMBackendError("ERR_CANCELLED", "fake mid-generation cancel")
        if b"BADJSON" in image_png:
            return "the model rambled and produced no json at all"
        confidence = 0.1 if b"LOWCONF" in image_png else 0.9
        return json.dumps(
            {
                "caption": f"A fake caption from device {self.plan.device}.",
                "explanation": "A fake explanation covering the key takeaway of the region.",
                "confidence": confidence,
            }
        )

    def unload(self) -> None:
        pass


def fake_aware_vision_factory(
    model_path: str,
    mmproj_path: str,
    plan: ResourcePlan,
    threads: int | None,
    family: str,
) -> VisionBackend:
    if is_fake_model(model_path):
        return FakeVisionBackend(plan)
    return LlamaVisionBackend(model_path, mmproj_path, plan, threads, family)
