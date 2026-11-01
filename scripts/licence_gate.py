from __future__ import annotations

import argparse
import importlib.metadata as md
import json
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from packaging.markers import default_environment
from packaging.requirements import Requirement
from packaging.utils import canonicalize_name

REPO = Path(__file__).resolve().parents[1]
PYPROJECT = REPO / "engine" / "pyproject.toml"
EXCEPTIONS = REPO / "engine" / "licence-exceptions.json"

ALLOWED = frozenset(
    {"mit", "apache-2.0", "bsd-2-clause", "bsd-3-clause", "bsd-4-clause", "bsd", "isc", "0bsd"}
)
ALIASES = {
    "mit license": "mit",
    "the mit license": "mit",
    "mit-0": "mit",
    "apache software license": "apache-2.0",
    "apache license 2.0": "apache-2.0",
    "apache license, version 2.0": "apache-2.0",
    "apache 2.0": "apache-2.0",
    "apache-2": "apache-2.0",
    "apache 2": "apache-2.0",
    "apache license": "apache-2.0",
    "bsd license": "bsd",
    "new bsd license": "bsd-3-clause",
    "bsd 3-clause": "bsd-3-clause",
    "bsd-3": "bsd-3-clause",
    "3-clause bsd": "bsd-3-clause",
    "isc license": "isc",
    "isc license (iscl)": "isc",
    "apache": "apache-2.0",
    "apache 2.0 license": "apache-2.0",
    "apache2.0": "apache-2.0",
    "apache license version 2.0": "apache-2.0",
    "3-clause bsd license": "bsd-3-clause",
    "bsd 3-clause license": "bsd-3-clause",
    "bsd-3-clause license": "bsd-3-clause",
    "bsd 2-clause license": "bsd-2-clause",
}


@dataclass
class Finding:
    name: str
    version: str
    licence: str
    verdict: str
    required_by: list[str] = field(default_factory=list)


def _normalise(term: str) -> str:
    cleaned = term.strip().lower()
    return ALIASES.get(cleaned, cleaned)


_OPERATORS = frozenset({"AND", "OR", "WITH"})
_TOKEN = re.compile(
    r"\s*(\(|\)|\b(?:AND|OR|WITH)\b|[^()]+?(?=\s*(?:\(|\)|\b(?:AND|OR|WITH)\b|$)))",
    re.IGNORECASE,
)


class _Malformed(ValueError):
    pass


def _tokens(text: str) -> list[str]:
    tokens: list[str] = []
    pos = 0
    while pos < len(text):
        match = _TOKEN.match(text, pos)
        if match is None or match.end() == pos:
            raise _Malformed(text)
        token = match.group(1).strip()
        if token.upper() in _OPERATORS:
            token = token.upper()
        tokens.append(token)
        pos = match.end()
    return tokens


class _Expression:
    def __init__(self, tokens: list[str]) -> None:
        self.tokens = tokens
        self.pos = 0

    def _peek(self) -> str | None:
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def _take(self) -> str:
        token = self._peek()
        if token is None:
            raise _Malformed("unexpected end")
        self.pos += 1
        return token

    def allowed(self) -> bool:
        value = self._any()
        if self._peek() is not None:
            raise _Malformed("trailing tokens")
        return value

    def _any(self) -> bool:
        value = self._all()
        while self._peek() == "OR":
            self._take()
            other = self._all()
            value = value or other
        return value

    def _all(self) -> bool:
        value = self._with()
        while self._peek() == "AND":
            self._take()
            other = self._with()
            value = value and other
        return value

    def _with(self) -> bool:
        value = self._primary()
        if self._peek() == "WITH":
            self._take()
            exception = self._take()
            if exception in _OPERATORS or exception in {"(", ")"}:
                raise _Malformed("WITH needs an exception id")
        return value

    def _primary(self) -> bool:
        token = self._take()
        if token == "(":
            value = self._any()
            if self._take() != ")":
                raise _Malformed("unclosed group")
            return value
        if token in _OPERATORS or token == ")":
            raise _Malformed(f"unexpected {token}")
        return _normalise(token) in ALLOWED


def licence_allowed(expression: str) -> bool:
    text = expression.strip()
    if not text:
        return False
    if _normalise(text) in ALLOWED:
        return True
    try:
        return _Expression(_tokens(text)).allowed()
    except _Malformed:
        return False


def declared_licence(dist: md.Distribution) -> str:
    meta = dist.metadata
    expression = (meta["License-Expression"] or "").strip()
    if expression:
        return expression
    classifiers = [
        c.split("::")[-1].strip()
        for c in meta.get_all("Classifier") or []
        if c.startswith("License ::") and c.split("::")[-1].strip() != "OSI Approved"
    ]
    raw = (meta["License"] or "").strip()
    if raw and len(raw) <= 80 and "\n" not in raw:
        return raw
    if classifiers:
        return " OR ".join(classifiers)
    return raw.splitlines()[0][:80] if raw else ""


def _pyproject() -> dict[str, Any]:
    return tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))


def _roots() -> list[Requirement]:
    return [Requirement(spec) for spec in _pyproject()["project"]["dependencies"]]


def _metadata_overrides() -> dict[tuple[str, str | None], list[str]]:
    entries = _pyproject().get("tool", {}).get("uv", {}).get("dependency-metadata", [])
    return {
        (canonicalize_name(entry["name"]), entry.get("version")): list(entry["requires-dist"])
        for entry in entries
    }


def runtime_closure() -> tuple[dict[str, md.Distribution], dict[str, set[str]], list[str]]:
    env = {key: str(value) for key, value in default_environment().items()}
    found: dict[str, md.Distribution] = {}
    parents: dict[str, set[str]] = {}
    missing: list[str] = []
    queue: list[tuple[Requirement, str]] = [(req, "txtcaster-engine") for req in _roots()]
    overrides = _metadata_overrides()
    seen_extras: dict[str, set[str]] = {}
    while queue:
        req, parent = queue.pop()
        name = canonicalize_name(req.name)
        parents.setdefault(name, set()).add(parent)
        extras = set(req.extras)
        if name in found and extras <= seen_extras.get(name, set()):
            continue
        try:
            dist = found.get(name) or md.distribution(req.name)
        except md.PackageNotFoundError:
            if parent == "txtcaster-engine":
                missing.append(req.name)
            continue
        found[name] = dist
        seen_extras.setdefault(name, set()).update(extras)
        requires = overrides.get((name, dist.version), overrides.get((name, None), dist.requires))
        for raw in requires or []:
            child = Requirement(raw)
            if child.marker is None:
                queue.append((child, name))
                continue
            wanted = extras or {""}
            if any(child.marker.evaluate({**env, "extra": extra}) for extra in wanted):
                queue.append((child, name))
    return found, parents, missing


def evaluate(exceptions: dict[str, dict[str, dict[str, str]]]) -> tuple[list[Finding], list[str]]:
    found, parents, missing = runtime_closure()
    permanent = exceptions.get("permanent", {})
    known = exceptions.get("known_violation", {})
    pending = exceptions.get("pending_owner", {})
    reviewed = exceptions.get("reviewed", {})
    findings: list[Finding] = []
    for name, dist in sorted(found.items()):
        licence = declared_licence(dist)
        review = reviewed.get(name)
        declares_expression = bool((dist.metadata["License-Expression"] or "").strip())
        if review and not declares_expression and review.get("version") == dist.version:
            licence = review["licence"]
        if name in permanent:
            verdict = "permanent_exception"
        elif name in known:
            verdict = "known_violation"
        elif name in pending:
            verdict = "pending_owner"
        elif licence_allowed(licence):
            verdict = "allowed"
        else:
            verdict = "violation"
        findings.append(
            Finding(name, dist.version, licence, verdict, sorted(parents.get(name, set())))
        )
    return findings, missing


def main() -> int:
    parser = argparse.ArgumentParser(description="engine runtime licence gate")
    parser.add_argument("--json", type=Path, default=None)
    args = parser.parse_args()
    exceptions = json.loads(EXCEPTIONS.read_text(encoding="utf-8"))
    findings, missing = evaluate(exceptions)
    for finding in findings:
        if finding.verdict != "allowed":
            print(
                f"{finding.verdict:20} {finding.name}=={finding.version} [{finding.licence}] "
                f"required by {', '.join(finding.required_by)}"
            )
    for name in missing:
        print(f"{'not_installed':20} {name} (declared runtime root, absent from this venv)")
    unexpected = [f for f in findings if f.verdict == "violation"]
    ruled = (
        set(exceptions.get("known_violation", {}))
        | set(exceptions.get("permanent", {}))
        | set(exceptions.get("reviewed", {}))
        | set(exceptions.get("pending_owner", {}))
    )
    stale = sorted(ruled - {f.name for f in findings})
    for name in stale:
        print(f"{'stale_ruling':20} {name} (no longer in the runtime closure)")
    allowed = sum(1 for f in findings if f.verdict == "allowed")
    print(
        f"licence gate: {len(findings)} runtime distributions, {allowed} allowed, "
        f"{len(unexpected)} unruled violation(s), {len(missing)} root(s) not installed"
    )
    if args.json is not None:
        args.json.write_text(
            json.dumps([f.__dict__ for f in findings], indent=2) + "\n", encoding="utf-8"
        )
    return 1 if unexpected or missing else 0


if __name__ == "__main__":
    sys.exit(main())
