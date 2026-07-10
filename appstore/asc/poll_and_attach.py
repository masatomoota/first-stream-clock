#!/usr/bin/env python3
"""Poll App Store Connect for the StreamClock macOS build to finish processing
(Delivery UUID d1507883-3466-47d3-880d-b5a5173a0fc6, CFBundleVersion "1",
CFBundleShortVersionString "1.0.0"), then attach it to appStoreVersion
342c9a64-21c1-4cea-89aa-8a969a7f26fb (MAC_OS, versionString 1.0.0).

Hard-scoped to app 6789441630 / bundleId net.firstcallmusic.streamclock only
-- safety_check() aborts if that app's bundleId doesn't match, before any
write is attempted. Never touches any other app.

Poll loop: GET /v1/builds?filter[app]=<APP_ID>, newest by uploadedDate, every
POLL_INTERVAL seconds. Prints every processingState change, including the
"no build visible yet" state sometimes seen right after `altool` upload,
before ASC's API has ingested it. Exits non-zero if the build reaches
FAILED/INVALID processing, or if MAX_WAIT elapses first.

Once VALID: PATCH /v1/appStoreVersions/{VERSION_ID}/relationships/build,
then re-GETs that relationship to verify the attach stuck.

Idempotent: if the version's relationships.build already points at the
current newest build for this app, this is a no-op (prints and exits 0
without any PATCH) -- safe to re-run, and does not even poll in that case.

Run with plain python3 (pyjwt + cryptography are already installed for the
system /usr/bin/python3 on this machine):

    python3 appstore/asc/poll_and_attach.py

On any API failure, prints the full JSON error body (status code + entire
decoded response, so nested errors[].associatedErrors are visible) and exits
non-zero.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from asc_common import BUNDLE_ID_IDENTIFIER, pretty, request  # noqa: E402

APP_ID = "6789441630"
VERSION_ID = "342c9a64-21c1-4cea-89aa-8a969a7f26fb"
POLL_INTERVAL = 60
MAX_WAIT = 45 * 60


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


def newest_build() -> dict | None:
    """Returns {id, version, state, uploadedDate} for the newest build of
    APP_ID (by uploadedDate), or None if no build is visible via the API yet."""
    status, body = request(
        "GET",
        "/builds",
        params={"filter[app]": APP_ID, "sort": "-uploadedDate", "limit": 10},
    )
    if status != 200:
        fatal("GET /builds", status, body)
    data = body.get("data", [])
    if not data:
        return None
    b = data[0]
    return {
        "id": b["id"],
        "version": b["attributes"].get("version"),
        "state": b["attributes"].get("processingState"),
        "uploadedDate": b["attributes"].get("uploadedDate"),
    }


def current_attached_build_id() -> str | None:
    status, body = request("GET", f"/appStoreVersions/{VERSION_ID}/relationships/build")
    if status != 200:
        fatal("GET appStoreVersions/relationships/build", status, body)
    data = body.get("data")
    return data["id"] if data else None


def wait_for_valid() -> dict:
    """Polls until the newest build for APP_ID is VALID. Prints every state
    change (including id changes). Raises SystemExit on FAILED/INVALID
    processing, or once MAX_WAIT elapses."""
    deadline = time.time() + MAX_WAIT
    last_key = None
    while True:
        b = newest_build()
        key = (b["id"], b["state"]) if b else ("NONE", "NO_BUILD_YET")
        if key != last_key:
            ts = time.strftime("%H:%M:%S")
            if b is None:
                print(f"[{ts}] POLL no build visible yet for app {APP_ID}", flush=True)
            else:
                print(
                    f"[{ts}] POLL build={b['id']} version={b['version']} "
                    f"processingState={b['state']} uploadedDate={b['uploadedDate']}",
                    flush=True,
                )
            last_key = key
        if b is not None:
            if b["state"] == "VALID":
                return b
            if b["state"] in ("FAILED", "INVALID"):
                status, body = request("GET", f"/builds/{b['id']}")
                raise SystemExit(
                    f"!! build {b['id']} finished processing as {b['state']!r} -- cannot "
                    f"attach. Full build record (HTTP {status}):\n{pretty(body)}"
                )
        if time.time() > deadline:
            raise SystemExit(
                f"!! TIMEOUT after {MAX_WAIT}s waiting for a VALID build on app {APP_ID} "
                f"(last observed: {b!r})"
            )
        time.sleep(POLL_INTERVAL)


def attach(build_id: str) -> None:
    status, body = request(
        "PATCH",
        f"/appStoreVersions/{VERSION_ID}/relationships/build",
        body={"data": {"type": "builds", "id": build_id}},
    )
    if status not in (200, 204):
        fatal(f"PATCH appStoreVersions/{VERSION_ID}/relationships/build", status, body)
    print(f"ATTACHED build={build_id} -> appStoreVersion={VERSION_ID}")


def verify(expected_build_id: str) -> None:
    status, body = request("GET", f"/appStoreVersions/{VERSION_ID}/relationships/build")
    if status != 200:
        fatal("GET appStoreVersions/relationships/build (verify)", status, body)
    got = (body.get("data") or {}).get("id")
    if got != expected_build_id:
        raise SystemExit(
            f"!! VERIFY FAILED: expected relationships.build.id == {expected_build_id!r}, "
            f"got {got!r}\n{pretty(body)}"
        )
    print(f"VERIFIED appStoreVersions/{VERSION_ID}.relationships.build == {got}")


def main() -> None:
    safety_check()
    print(f"== app {APP_ID} ({BUNDLE_ID_IDENTIFIER}) -- version {VERSION_ID} ==")

    already = current_attached_build_id()
    latest = newest_build()

    if already is not None and latest is not None and already == latest["id"]:
        print(
            f"OK   appStoreVersions/{VERSION_ID} already has build {already} attached "
            f"(version={latest['version']} processingState={latest['state']}) -- nothing to do."
        )
        return

    if already is not None:
        print(
            f"NOTE version currently has build {already} attached, which is not the "
            f"newest build ({latest['id'] if latest else 'unknown'}). Will attach the "
            f"newest VALID build instead."
        )

    build = wait_for_valid()
    attach(build["id"])
    verify(build["id"])
    print(
        f"DONE build={build['id']} version={build['version']} "
        f"processingState={build['state']} attached to appStoreVersion={VERSION_ID}"
    )


if __name__ == "__main__":
    main()
