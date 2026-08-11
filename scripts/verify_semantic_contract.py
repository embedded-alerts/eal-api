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

    main_rs = "\n".join(path.read_text() for path in sorted((ROOT / "src").glob("main*.rs")))
    for route in required_paths:
        axum_route = route.replace("{id}", "{id}")
        require(
            f'.route("{axum_route}"' in main_rs,
            f"Axum router is missing {route}",
        )

    extractor = "\n".join(path.read_text() for path in sorted((ROOT / "src/semantic").glob("extract*.rs")))
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

    crawler = "\n".join(path.read_text() for path in sorted((ROOT / "src/semantic").glob("crawl*.rs")))
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

    repository_text = "\n".join(
        path.read_text(errors="ignore")
        for path in ROOT.rglob("*")
        if path.is_file() and ".git" not in path.parts
    ).lower()
    require("next.js" not in repository_text or "there is no next.js" in repository_text,
            "Next.js implementation text was introduced")

    print("semantic contract verified")


if __name__ == "__main__":
    main()
