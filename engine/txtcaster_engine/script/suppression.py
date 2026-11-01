"""What was removed from the narration, and on what evidence.

Suppression is destructive and the product is local-only, so a wrong deletion is silent
and there is no telemetry that would ever surface it. The remedy is not to make false
positives rare and hope — it is to make every one of them visible to the person holding
the book. Every suppressed block is recorded with the evidence that condemned it, so the
player can list them and the reader can check.

Records carry a certainty. `CERTAIN` is furniture both the evidence and the shape agree
on. `UNCERTAIN` is a block the layout model called furniture while it reads as prose:
still suppressed, because a model that looked at the page outranks a text heuristic that
did not, but surfaced first so a disagreement never passes unnoticed.
"""

from dataclasses import dataclass
from enum import StrEnum

from txtcaster_engine.ingest.headers import ROLE_BY_LABEL
from txtcaster_engine.script.canonical import CanonicalPage
from txtcaster_engine.script.shape import looks_like_prose

SUPPRESSED_ROLES = frozenset({"header", "footer"})
ARTIFACT_LABEL_PREFIX = "artifact_"


class Certainty(StrEnum):
    CERTAIN = "certain"
    UNCERTAIN = "uncertain"


@dataclass(frozen=True)
class Suppressed:
    page_no: int
    role: str
    reason: str
    certainty: Certainty
    text: str
    words: int

    def to_json(self) -> dict[str, object]:
        return {
            "page_no": self.page_no,
            "role": self.role,
            "reason": self.reason,
            "certainty": self.certainty.value,
            "text": self.text,
            "words": self.words,
        }


def collect_suppressed(pages: list[CanonicalPage]) -> list[Suppressed]:
    records: list[Suppressed] = []
    for page in pages:
        for block in page.blocks:
            if block.role not in SUPPRESSED_ROLES:
                continue
            labelled = (block.label or "") in ROLE_BY_LABEL
            if (block.label or "").startswith(ARTIFACT_LABEL_PREFIX):
                reason = "artifact-mark"
            else:
                reason = "layout-label" if labelled else "repetition"
            certainty = (
                Certainty.UNCERTAIN
                if reason == "layout-label" and looks_like_prose(block.text)
                else Certainty.CERTAIN
            )
            records.append(
                Suppressed(
                    page_no=page.page_no,
                    role=block.role,
                    reason=reason,
                    certainty=certainty,
                    text=" ".join(block.text.split()),
                    words=len(block.text.split()),
                )
            )
    return records
