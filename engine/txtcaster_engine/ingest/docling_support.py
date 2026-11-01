from dataclasses import dataclass
from pathlib import Path

LAYOUT_REPO_FOLDER = "docling-project--docling-layout-heron"
TABLEFORMER_REPO_FOLDER = "docling-project--docling-models"
RAPIDOCR_REPO_FOLDER = "RapidOcr"

_TABLEFORMER_DIR = "model_artifacts/tableformer"

_REQUIRED_PATHS = (
    Path(LAYOUT_REPO_FOLDER) / "model.safetensors",
    Path(LAYOUT_REPO_FOLDER) / "config.json",
    Path(LAYOUT_REPO_FOLDER) / "preprocessor_config.json",
    Path(TABLEFORMER_REPO_FOLDER) / _TABLEFORMER_DIR / "fast/tableformer_fast.safetensors",
    Path(TABLEFORMER_REPO_FOLDER) / _TABLEFORMER_DIR / "fast/tm_config.json",
    Path(TABLEFORMER_REPO_FOLDER) / _TABLEFORMER_DIR / "accurate/tableformer_accurate.safetensors",
    Path(TABLEFORMER_REPO_FOLDER) / _TABLEFORMER_DIR / "accurate/tm_config.json",
    Path(RAPIDOCR_REPO_FOLDER) / "torch/PP-OCRv4/det/en_PP-OCRv3_det_mobile.pth",
    Path(RAPIDOCR_REPO_FOLDER) / "torch/PP-OCRv4/cls/ch_ptocr_mobile_v2.0_cls_mobile.pth",
    Path(RAPIDOCR_REPO_FOLDER) / "torch/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile.pth",
    Path(RAPIDOCR_REPO_FOLDER) / "paddle/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile/en_dict.txt",
    Path(RAPIDOCR_REPO_FOLDER) / "resources/fonts/FZYTK.TTF",
)


def docling_available() -> bool:
    try:
        import docling.document_converter  # noqa: F401
    except ImportError:
        return False
    return True


@dataclass(frozen=True)
class DoclingArtifacts:
    root: Path

    def missing_files(self) -> list[str]:
        return [rel.as_posix() for rel in _REQUIRED_PATHS if not (self.root / rel).is_file()]

    def is_complete(self) -> bool:
        return not self.missing_files()


def check_artifacts(artifacts_dir: str) -> DoclingArtifacts:
    return DoclingArtifacts(root=Path(artifacts_dir))
