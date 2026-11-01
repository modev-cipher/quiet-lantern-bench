import re

_EOL_HYPHEN_RE = re.compile(r"(\w)-\n(\w)")


def _join(m: re.Match[str]) -> str:
    return m.group(1) + m.group(2) if m.group(2).islower() else m.group(0)


def dehyphenate_block(text: str) -> str:
    return _EOL_HYPHEN_RE.sub(_join, text)
