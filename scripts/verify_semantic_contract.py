#!/usr/bin/env python3
"""Fast structural checks for the DEN-3461 semantic indexing contract."""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def joined_sources(directory: Path, pattern: str) -> str:
    """Read only checked-in contract sources, never build/cache output."""
    return "\n".join(path.read_text() for path in sorted(directory.glob(pattern)))


def verify_no_nextjs_runtime() -> None:
    for config_name in ("next.config.js", "next.config.mjs", "next.config.ts"):
        require(
            not (ROOT / config_name).exists(),
            f"Next.js runtime config must not be introduced: {config_name}",
        )

    package_manifest = ROOT / "package.json"
    if not package_manifest.exists():
        return

    package = json.loads(package_manifest.read_text())
    dependencies = {
        **package.get("dependencies", {}),
        **package.get("devDependencies", {}),
        **package.get("peerDependencies", {}),
    }
    require("next" not in dependencies, "Next.js must not be added as a dependency")


def main() -> None:
    openapi = json.loads((ROOT / "openapi/eal-api.json").read_text())
    required_paths = {
        "/v1/sources": {"get", "post"},
        "/v1/sources/{id}/scan": {"post"},
        "/v1/sources/{id}/ingest": {"post"},
        "/v1/pages": {"get"},
        "/v1/embeddings/search": {"post"},
        "/v1/matches": {"get"},
    }
    for path, methods in required_paths.items():
        require(path in openapi["paths"], f"OpenAPI is missing {path}")
        require(
            methods <= set(openapi["paths"][path]),
            f"OpenAPI is missing methods for {path}: {methods}",
        )

    main_rs = joined_sources(ROOT / "src", "main*.rs")
    for route in required_paths:
        require(
            f'.route("{route}"' in main_rs,
            f"Axum router is missing {route}",
        )

    extractor = joined_sources(ROOT / "src/semantic", "extract*.rs")
    for segment_kind in [
        "Title",
        "Heading",
        "Summary",
        "Sentence",
        "Entity",
        "Keyword",
        "UrlSignal",
        "Query",
    ]:
        require(
            re.search(rf"\b{segment_kind}\b", extractor) is not None,
            f"extractor is missing {segment_kind} segment support",
        )

    crawler = joined_sources(ROOT / "src/semantic", "crawl*.rs")
    for guard in [
        ".no_proxy()",
        "Policy::none()",
        "resolve_to_addrs",
        "is_public_ip",
        "MAX_REDIRECTS",
        "content_length",
    ]:
        require(guard in crawler, f"crawler is missing guard: {guard}")

    migration = (ROOT / "migrations/20260810223000_semantic_page_index.sql").read_text()
    for table in [
        "eal_source_domains",
        "eal_source_items",
        "eal_source_item_revisions",
        "eal_embedding_sets",
        "eal_embedding_segments",
        "eal_match_candidates",
    ]:
        require(
            f"CREATE TABLE IF NOT EXISTS {table}" in migration,
            f"migration is missing {table}",
        )
        require(
            f"ALTER TABLE {table} ENABLE ROW LEVEL SECURITY" in migration,
            f"migration is missing RLS for {table}",
        )

    verify_no_nextjs_runtime()
    print("semantic contract verified")


if __name__ == "__main__":
    main()
