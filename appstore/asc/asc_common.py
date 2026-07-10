"""Shared App Store Connect API helpers (JWT auth + a small requests wrapper).

Usage: run scripts with
    uv run --with pyjwt --with cryptography appstore/asc/register_bundle_id.py

Credentials are NOT stored in this file — this repository is public. They are read from
the environment, or from `appstore/asc/credentials.env` (gitignored), which must define:

    ASC_KEY_ID=<10-char key id from App Store Connect > Users and Access > Integrations>
    ASC_ISSUER_ID=<uuid shown above the key table on that same page>

The private key itself lives outside the repo, at
`~/.appstoreconnect/private_keys/AuthKey_<ASC_KEY_ID>.p8` (override with ASC_KEY_PATH).
The issuer id is not retrievable from any API; a human reads it once from the web UI.
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import jwt

def _load_credentials_env() -> None:
    """Seed os.environ from appstore/asc/credentials.env, without overriding real env vars."""
    path = Path(__file__).with_name("credentials.env")
    if not path.is_file():
        return
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        os.environ.setdefault(key.strip(), value.strip())


_load_credentials_env()


def _required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise SystemExit(
            f"!! {name} is not set. Put it in appstore/asc/credentials.env "
            f"(gitignored) or export it. See the module docstring."
        )
    return value


ASC_KEY_ID = _required("ASC_KEY_ID")
ASC_ISSUER_ID = _required("ASC_ISSUER_ID")
ASC_KEY_PATH = os.environ.get(
    "ASC_KEY_PATH",
    str(Path.home() / ".appstoreconnect" / "private_keys" / f"AuthKey_{ASC_KEY_ID}.p8"),
)

API_BASE = "https://api.appstoreconnect.apple.com/v1"

TEAM_ID = "HS63RU33N9"
BUNDLE_ID_IDENTIFIER = "net.firstcallmusic.streamclock"
BUNDLE_ID_NAME = "StreamClock"


def _load_private_key() -> str:
    path = Path(ASC_KEY_PATH)
    if not path.is_file():
        raise SystemExit(f"!! ASC private key not found: {path}")
    return path.read_text()


def make_token(expires_in: int = 60 * 15) -> str:
    """Builds a short-lived ES256 JWT for the App Store Connect API."""
    private_key = _load_private_key()
    now = int(time.time())
    payload = {
        "iss": ASC_ISSUER_ID,
        "iat": now,
        "exp": now + expires_in,
        "aud": "appstoreconnect-v1",
    }
    headers = {"kid": ASC_KEY_ID, "typ": "JWT"}
    token = jwt.encode(payload, private_key, algorithm="ES256", headers=headers)
    # PyJWT >= 2 returns str already; older versions return bytes.
    if isinstance(token, bytes):
        token = token.decode("utf-8")
    return token


def request(
    method: str,
    path: str,
    *,
    params: dict[str, Any] | None = None,
    body: dict[str, Any] | None = None,
) -> tuple[int, dict[str, Any] | None]:
    """Makes an authenticated request to the App Store Connect API.

    `path` may be a full URL (for pagination `links.next`) or a path like
    "/bundleIds" that gets appended to API_BASE.
    Returns (status_code, decoded_json_or_None).
    """
    if path.startswith("http"):
        url = path
    else:
        url = f"{API_BASE}{path}"

    if params:
        from urllib.parse import urlencode

        # Support repeated keys like filter[foo] naturally via list values.
        qs = urlencode(params, doseq=True)
        sep = "&" if "?" in url else "?"
        url = f"{url}{sep}{qs}"

    data = None
    headers = {
        "Authorization": f"Bearer {make_token()}",
        "Accept": "application/json",
    }
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"

    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req) as resp:
            raw = resp.read()
            status = resp.status
    except urllib.error.HTTPError as e:
        raw = e.read()
        status = e.code

    if not raw:
        return status, None
    try:
        return status, json.loads(raw)
    except json.JSONDecodeError:
        return status, {"raw": raw.decode("utf-8", "replace")}


def get_all_pages(path: str, *, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    """GETs `path` and follows links.next pagination, returning all `data` items."""
    items: list[dict[str, Any]] = []
    status, body = request("GET", path, params=params)
    if status >= 300:
        raise SystemExit(f"!! GET {path} failed: {status} {body}")
    items.extend(body.get("data", []))
    next_url = body.get("links", {}).get("next")
    while next_url:
        status, body = request("GET", next_url)
        if status >= 300:
            raise SystemExit(f"!! GET {next_url} failed: {status} {body}")
        items.extend(body.get("data", []))
        next_url = body.get("links", {}).get("next")
    return items


def pretty(obj: Any) -> str:
    return json.dumps(obj, indent=2, ensure_ascii=False)
