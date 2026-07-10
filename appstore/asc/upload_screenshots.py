#!/usr/bin/env python3
"""Upload the StreamClock macOS App Store screenshots via the ASC API.

Uploads, in this exact order:
    appstore/screenshots/mac/streamclock-macos-1.png
    appstore/screenshots/mac/streamclock-macos-2.png
    appstore/screenshots/mac/streamclock-macos-3.png

...as the APP_DESKTOP appScreenshotSet of the "ja" appStoreVersionLocalization
of appStoreVersion 342c9a64-21c1-4cea-89aa-8a969a7f26fb (MAC_OS, 1.0.0). All
three are 2880x1800, 8-bit RGB, no alpha (verified with `sips` before writing
this script) -- an accepted APP_DESKTOP size.

Hard-scoped to app 6789441630 / bundleId net.firstcallmusic.streamclock only
-- safety_check() aborts if that app's bundleId doesn't match, before any
write is attempted. Never touches any other app.

4-step upload protocol per screenshot (see "Uploading Assets to App Store
Connect" in Apple's API docs):
    1. POST /v1/appScreenshotSets   (create-or-reuse; screenshotDisplayType=APP_DESKTOP,
       linked to the "ja" appStoreVersionLocalization)
    2. POST /v1/appScreenshots      (reserve: fileName + fileSize -> id + uploadOperations)
    3. For each uploadOperations entry, issue the given `method` against `url` with the
       given `requestHeaders`, body = data[offset:offset+length] -- applied verbatim,
       no headers of our own added.
    4. PATCH /v1/appScreenshots/{id}  {"uploaded": true, "sourceFileChecksum": "<md5 of whole file>"}

Then polls each screenshot's assetDeliveryState until state == "COMPLETE" (or
FAILED / ASSET_POLL_TIMEOUT) and asserts assetDeliveryState.errors is empty.

Idempotent: reuses an existing APP_DESKTOP set on the localization if one
already exists, and skips re-uploading (but still polls/verifies) any file
whose fileName is already present in the set -- safe to re-run after a
partial failure.

Note on ordering: the set starts out empty for this app (verified live before
writing this script), and screenshots are reserved strictly in FILES order,
waiting for each one to fully complete before starting the next -- so the set
ends up in the right order without needing an explicit reorder call. (An ASC
relationship-replace endpoint for reordering may exist, but this script
deliberately does not call it: the exact request shape could not be verified
against a trustworthy source, and calling an unverified endpoint risks a
false failure on an otherwise-successful upload.) The final verification
step re-GETs the set and prints the API's own order so this assumption is
checked, not just assumed.

Run with plain python3 (pyjwt + cryptography are already installed for the
system /usr/bin/python3 on this machine):

    python3 appstore/asc/upload_screenshots.py

On any API failure, prints the full JSON error body (status + entire decoded
response, so nested errors[].associatedErrors are visible) and exits non-zero.
"""

from __future__ import annotations

import hashlib
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from asc_common import BUNDLE_ID_IDENTIFIER, get_all_pages, pretty, request  # noqa: E402

APP_ID = "6789441630"
VERSION_ID = "342c9a64-21c1-4cea-89aa-8a969a7f26fb"
TARGET_LOCALE = "ja"
DISPLAY_TYPE = "APP_DESKTOP"
SCREENSHOT_DIR = Path(__file__).resolve().parent.parent / "screenshots" / "mac"
FILES = ["streamclock-macos-1.png", "streamclock-macos-2.png", "streamclock-macos-3.png"]

ASSET_POLL_INTERVAL = 5
ASSET_POLL_TIMEOUT = 600


def fatal(context: str, status: int, body) -> None:
    raise SystemExit(f"!! {context} failed: HTTP {status}\n{pretty(body)}")


def safety_check() -> None:
    """Refuses to run against any app other than the one this script is scoped to."""
    status, body = request("GET", f"/apps/{APP_ID}")
    if status != 200:
        fatal(f"GET /apps/{APP_ID}", status, body)
    bundle_id = body["data"]["attributes"]["bundleId"]
    if bundle_id != BUNDLE_ID_IDENTIFIER:
        raise SystemExit(
            f"!! SAFETY ABORT: /apps/{APP_ID} has bundleId {bundle_id!r}, expected "
            f"{BUNDLE_ID_IDENTIFIER!r}. Refusing to touch an app that doesn't match "
            f"asc_common.BUNDLE_ID_IDENTIFIER."
        )


def find_localization(version_id: str, locale: str) -> str:
    items = get_all_pages(f"/appStoreVersions/{version_id}/appStoreVersionLocalizations", params={"limit": 50})
    matches = [i for i in items if i["attributes"]["locale"] == locale]
    if len(matches) != 1:
        found = [(i["id"], i["attributes"]["locale"]) for i in items]
        raise SystemExit(
            f"!! expected exactly 1 {locale!r} appStoreVersionLocalization on version "
            f"{version_id}, found {len(matches)}: {found}"
        )
    return matches[0]["id"]


def find_or_create_set(loc_id: str, display_type: str) -> str:
    for s in get_all_pages(f"/appStoreVersionLocalizations/{loc_id}/appScreenshotSets", params={"limit": 50}):
        if s["attributes"].get("screenshotDisplayType") == display_type:
            print(f"OK     reusing appScreenshotSet {s['id']} ({display_type})")
            return s["id"]
    status, body = request(
        "POST",
        "/appScreenshotSets",
        body={
            "data": {
                "type": "appScreenshotSets",
                "attributes": {"screenshotDisplayType": display_type},
                "relationships": {
                    "appStoreVersionLocalization": {"data": {"type": "appStoreVersionLocalizations", "id": loc_id}}
                },
            }
        },
    )
    if status not in (200, 201):
        fatal("POST /appScreenshotSets", status, body)
    sid = body["data"]["id"]
    print(f"CREATE appScreenshotSet {sid} ({display_type})")
    return sid


def existing_screenshots(set_id: str) -> dict[str, dict]:
    items = get_all_pages(f"/appScreenshotSets/{set_id}/appScreenshots", params={"limit": 50})
    return {i["attributes"]["fileName"]: i for i in items}


def reserve(set_id: str, path: Path, data: bytes) -> dict:
    status, body = request(
        "POST",
        "/appScreenshots",
        body={
            "data": {
                "type": "appScreenshots",
                "attributes": {"fileSize": len(data), "fileName": path.name},
                "relationships": {"appScreenshotSet": {"data": {"type": "appScreenshotSets", "id": set_id}}},
            }
        },
    )
    if status not in (200, 201):
        fatal(f"POST /appScreenshots (reserve {path.name})", status, body)
    return body["data"]


def do_upload_operations(screenshot: dict, data: bytes, filename: str) -> None:
    ops = screenshot["attributes"].get("uploadOperations") or []
    if not ops:
        raise SystemExit(f"!! no uploadOperations returned for {filename}:\n{pretty(screenshot)}")
    for op in ops:
        offset, length = op["offset"], op["length"]
        chunk = data[offset:offset + length]
        req = urllib.request.Request(op["url"], data=chunk, method=op["method"])
        for h in op.get("requestHeaders", []):
            req.add_header(h["name"], h["value"])
        try:
            with urllib.request.urlopen(req) as resp:
                status = resp.status
        except urllib.error.HTTPError as e:
            body_text = e.read().decode("utf-8", "replace")
            raise SystemExit(
                f"!! upload operation failed for {filename} (offset={offset} length={length}): "
                f"HTTP {e.code}\n{body_text}"
            )
        if status not in (200, 201, 204):
            raise SystemExit(
                f"!! upload operation failed for {filename} (offset={offset} length={length}): HTTP {status}"
            )


def commit(screenshot_id: str, md5: str, filename: str) -> None:
    status, body = request(
        "PATCH",
        f"/appScreenshots/{screenshot_id}",
        body={
            "data": {
                "type": "appScreenshots",
                "id": screenshot_id,
                "attributes": {"uploaded": True, "sourceFileChecksum": md5},
            }
        },
    )
    if status != 200:
        fatal(f"PATCH /appScreenshots/{screenshot_id} (commit {filename})", status, body)


def poll_until_complete(screenshot_id: str, filename: str) -> dict:
    deadline = time.time() + ASSET_POLL_TIMEOUT
    last_state = None
    while True:
        status, body = request("GET", f"/appScreenshots/{screenshot_id}")
        if status != 200:
            fatal(f"GET /appScreenshots/{screenshot_id} (poll {filename})", status, body)
        attrs = body["data"]["attributes"]
        ads = attrs.get("assetDeliveryState") or {}
        state = ads.get("state")
        if state != last_state:
            print(f"  POLL {filename} ({screenshot_id}) assetDeliveryState={state}", flush=True)
            last_state = state
        if state == "COMPLETE":
            return ads
        if state == "FAILED":
            raise SystemExit(f"!! {filename} ({screenshot_id}) asset delivery FAILED:\n{pretty(ads)}")
        if time.time() > deadline:
            raise SystemExit(
                f"!! TIMEOUT after {ASSET_POLL_TIMEOUT}s waiting for {filename} ({screenshot_id}) "
                f"to reach COMPLETE (last state: {state!r})"
            )
        time.sleep(ASSET_POLL_INTERVAL)


def upload_one(set_id: str, filename: str) -> tuple[str, dict]:
    path = SCREENSHOT_DIR / filename
    if not path.is_file():
        raise SystemExit(f"!! screenshot file not found: {path}")
    data = path.read_bytes()

    existing = existing_screenshots(set_id)
    if filename in existing:
        sid = existing[filename]["id"]
        print(f"OK     {filename} already present in set as {sid} -- skipping upload, will still verify")
    else:
        screenshot = reserve(set_id, path, data)
        sid = screenshot["id"]
        print(f"CREATE {filename} -> reserved as {sid} ({len(data)} bytes)")
        do_upload_operations(screenshot, data, filename)
        md5 = hashlib.md5(data).hexdigest()
        commit(sid, md5, filename)
        print(f"       uploaded + committed {filename} (md5 {md5})")

    ads = poll_until_complete(sid, filename)
    errors = ads.get("errors") or []
    if errors:
        raise SystemExit(f"!! {filename} ({sid}) completed with non-empty assetDeliveryState.errors:\n{pretty(errors)}")
    print(f"DONE   {filename}: id={sid} assetDeliveryState.state=COMPLETE errors=[]")
    return sid, ads


def main() -> None:
    safety_check()
    print(f"== app {APP_ID} ({BUNDLE_ID_IDENTIFIER}) -- version {VERSION_ID} ==")

    loc_id = find_localization(VERSION_ID, TARGET_LOCALE)
    print(f"== localization[{TARGET_LOCALE}] = {loc_id} ==")

    set_id = find_or_create_set(loc_id, DISPLAY_TYPE)

    results = []
    for filename in FILES:
        sid, ads = upload_one(set_id, filename)
        results.append((filename, sid, ads))

    print("\n" + "=" * 22 + " VERIFICATION (fresh GET of set) " + "=" * 22)
    final_items = get_all_pages(f"/appScreenshotSets/{set_id}/appScreenshots", params={"limit": 50})
    order = [i["attributes"]["fileName"] for i in final_items]
    print(f"appScreenshotSet {set_id} ({DISPLAY_TYPE}) now contains, in API-returned order: {order}")
    if order != FILES:
        print(f"!! WARNING: order {order} does not match intended upload order {FILES}")

    print("\nSUMMARY")
    for filename, sid, ads in results:
        print(f"  {filename}: id={sid} assetDeliveryState={ads}")

    print("\nDONE. All screenshots uploaded/verified.")


if __name__ == "__main__":
    main()
