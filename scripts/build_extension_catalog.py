#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: Mirrors the Pi community Worker index into Picot's GitHub catalog file.
# ABOUTME: Projects each record down to the fields the Settings UI consumes.

"""Generate the community extension catalog consumed by Picot.

The catalog is a snapshot of the complete paginated response from
pi-packages-api.shixin.workers.dev, projected down to the fields the
Settings > Extensions community page actually reads, and written as
minified JSON. The file is committed to this repository so the UI can
fetch it from raw.githubusercontent.com instead of querying the Worker
at runtime.

Usage:
    python3 scripts/build_extension_catalog.py

Run manually when refreshing the catalog, review the generated JSON
diff, then commit and push it to the ``main`` branch.
No secrets or third-party dependencies are required.
"""

from __future__ import annotations

import json
import sys
import time
from datetime import UTC, datetime
from http.client import HTTPException
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
COMMUNITY_OUTPUT = REPOSITORY_ROOT / "community-extensions.json"
WORKER_PACKAGES_URL = "https://pi-packages-api.shixin.workers.dev/packages"
WORKER_PAGE_SIZE = 250
USER_AGENT = "picot-extension-catalog/1.0"
REQUEST_RETRY_DELAYS_SECONDS = (1, 3, 9)

# Fields kept per package record. Everything else (keywords, publisher,
# maintainers, version, video, image, …) is dead weight for the UI and is
# dropped to keep the committed file small.
PACKAGE_FIELDS = ("name", "description", "author", "types", "downloads", "date")
LINK_FIELDS = ("npm", "repository", "homepage")


def fetch_json(url: str) -> Any:
    if urlparse(url).scheme != "https":
        raise RuntimeError(f"refusing to open non-https url: {url}")
    # pi-lens-ignore: S310
    request = Request(url, headers={"Accept": "application/json", "User-Agent": USER_AGENT})
    for attempt, delay in enumerate((*REQUEST_RETRY_DELAYS_SECONDS, None), start=1):
        # pi-lens-ignore: S310
        try:
            with urlopen(request, timeout=60) as response:
                return json.load(response)
        except (HTTPError, HTTPException, URLError, TimeoutError, json.JSONDecodeError) as error:
            if delay is None:
                raise RuntimeError(f"request failed for {url}: {error}") from error
            print(
                f"request attempt {attempt} failed; retrying in {delay}s: {error}",
                file=sys.stderr,
            )
            time.sleep(delay)
    raise AssertionError("retry loop must return or raise")


def fetch_community_catalog() -> tuple[list[dict[str, Any]], str | None]:
    """Fetch every Worker page without altering the Worker record schema."""
    packages: list[dict[str, Any]] = []
    page = 1
    total_pages = 1
    updated_at: str | None = None
    while page <= total_pages:
        query = urlencode({"page": page, "pageSize": WORKER_PAGE_SIZE})
        payload = fetch_json(f"{WORKER_PACKAGES_URL}?{query}")
        if not isinstance(payload, dict) or not isinstance(payload.get("packages"), list):
            raise RuntimeError(f"Worker returned an invalid packages response on page {page}")
        if page == 1:
            raw_total_pages = payload.get("totalPages", 1)
            try:
                total_pages = int(raw_total_pages)
            except (TypeError, ValueError) as error:
                raise RuntimeError(
                    f"Worker returned an invalid totalPages value: {raw_total_pages!r}"
                ) from error
            updated_at = payload.get("updatedAt") if isinstance(payload.get("updatedAt"), str) else None
        packages.extend(package for package in payload["packages"] if isinstance(package, dict))
        page += 1
    return packages, updated_at


def project_package(package: dict[str, Any]) -> dict[str, Any]:
    """Keep only the catalog fields the community page renders."""
    projected = {field: package[field] for field in PACKAGE_FIELDS if field in package}
    links = package.get("links")
    if isinstance(links, dict):
        kept_links = {field: links[field] for field in LINK_FIELDS if links.get(field)}
        if kept_links:
            projected["links"] = kept_links
    return projected


def write_json(path: Path, payload: dict[str, Any]) -> None:
    # Minified: the catalog is fetched whole by every client, so byte size
    # directly drives load time; the committed diff stays reviewable by name.
    content = json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + "\n"
    temporary_path = path.with_suffix(f"{path.suffix}.tmp")
    temporary_path.write_text(content, encoding="utf-8")
    temporary_path.replace(path)


def main() -> int:
    packages, source_updated_at = fetch_community_catalog()
    generated_at = datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    projected = [project_package(package) for package in packages]
    write_json(
        COMMUNITY_OUTPUT,
        {
            "version": 1,
            "generatedAt": generated_at,
            "source": {"url": WORKER_PACKAGES_URL, "updatedAt": source_updated_at},
            "projection": {
                "packageFields": list(PACKAGE_FIELDS),
                "linkFields": list(LINK_FIELDS),
                "retainedPackages": "all",
            },
            "packages": projected,
        },
    )
    print(f"wrote {len(projected)} community packages to {COMMUNITY_OUTPUT.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
