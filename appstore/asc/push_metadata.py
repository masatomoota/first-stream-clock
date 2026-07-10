#!/usr/bin/env python3
"""Push App Store Connect metadata for StreamClock (macOS, app id 6789441630)
from ../metadata.json, which is the single source of truth for description,
keywords, URLs, review contact, age rating, copyright, and categories.

Idempotent / safe to re-run: every ensure_* function GETs the current state
first and only PATCHes the attributes that actually differ, or POSTs a new
sub-resource only when one doesn't already exist for that locale. Re-running
with no changes pending will just print "OK" lines and touch nothing.

Prints every change it makes. Each step (and each locale within a
multi-locale step) is isolated: a failure on one is printed in full and
recorded, but does not stop the other independent steps from running (so one
known, real-world blocker -- e.g. a rejected locale -- doesn't hide progress
on everything else). If anything failed, the script exits non-zero at the
end, after printing a summary with the full API error body for each failure.

Run with plain python3 (pyjwt + cryptography are already installed for the
system /usr/bin/python3 on this machine):

    python3 appstore/asc/push_metadata.py

Safety: this script hard-checks that /v1/apps/{appId}'s bundleId matches
asc_common.BUNDLE_ID_IDENTIFIER before making any write, and aborts instead of
touching a different app.

Note on ageRatingDeclarations: the live schema has more required attributes
than metadata.json's "ageRating" object (Apple expanded the age-rating
questionnaire since metadata.json was written). See AGE_RATING_EXTRA_FIELDS
below — discovered live via a 409 ENTITY_ERROR.ATTRIBUTE.REQUIRED response and
cross-checked against Apple's published attribute list at
https://developer.apple.com/documentation/appstoreconnectapi/ageratingdeclaration/attributes-data.dictionary
All of them are "not applicable" answers (false / "NONE"), appropriate for a
clock/timecode utility with no ads, chat, user content, loot boxes, weapons,
health topics, parental-control feature, or age-assurance mechanism.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from asc_common import BUNDLE_ID_IDENTIFIER, get_all_pages, pretty, request  # noqa: E402

METADATA_PATH = Path(__file__).resolve().parent.parent / "metadata.json"

# See module docstring. Keys not present in metadata.json's ageRating object
# but required by the live API as of 2026-07-10.
AGE_RATING_EXTRA_FIELDS: dict[str, object] = {
    "advertising": False,
    "ageAssurance": False,
    "gunsOrOtherWeapons": "NONE",
    "healthOrWellnessTopics": False,
    "lootBox": False,
    "messagingAndChat": False,
    "parentalControls": False,
    "userGeneratedContent": False,
}


class ApiError(Exception):
    """Raised by fatal() for a single step/locale; caught and recorded so
    other independent steps still run. Pre-flight sanity checks (wrong app,
    wrong resource counts) still use SystemExit directly to hard-abort."""


FAILURES: list[str] = []


def fatal(context: str, status: int, body: dict | None) -> None:
    raise ApiError(f"{context} failed: HTTP {status}\n{pretty(body)}")


def run_step(label: str, fn, *args) -> None:
    """Runs one step, isolating failures so the rest of main() still runs."""
    try:
        fn(*args)
    except ApiError as e:
        print(f"!! {label}: {e}")
        FAILURES.append(label)


def load_metadata() -> dict:
    """metadata.json, with the gitignored review contact merged back in.

    `reviewDetails` lives in `appstore/review-contact.json` because it carries a personal
    phone number and this repository is public.
    """
    meta = json.loads(METADATA_PATH.read_text(encoding="utf-8"))
    contact = METADATA_PATH.with_name("review-contact.json")
    if contact.is_file():
        meta["reviewDetails"] = json.loads(contact.read_text(encoding="utf-8"))["reviewDetails"]
    return meta


def diff_attrs(current: dict, desired: dict) -> dict:
    """Returns the subset of `desired` whose value differs from `current`."""
    return {k: v for k, v in desired.items() if current.get(k) != v}


# ---------------------------------------------------------------------------
# Resolve the app / appInfo / version this script is allowed to touch.
# ---------------------------------------------------------------------------


def get_app(app_id: str) -> dict:
    status, body = request("GET", f"/apps/{app_id}")
    if status != 200:
        fatal(f"GET /apps/{app_id}", status, body)
    return body["data"]


def get_single_app_info(app_id: str) -> dict:
    items = get_all_pages(f"/apps/{app_id}/appInfos", params={"limit": 50})
    if len(items) != 1:
        raise SystemExit(f"!! expected exactly 1 appInfo for app {app_id}, found {len(items)}: {[i['id'] for i in items]}")
    return items[0]


def get_single_version(app_id: str, platform: str, version_string: str) -> dict:
    items = get_all_pages(f"/apps/{app_id}/appStoreVersions", params={"limit": 50})
    matches = [v for v in items if v["attributes"]["platform"] == platform and v["attributes"]["versionString"] == version_string]
    if len(matches) != 1:
        found = [(v["id"], v["attributes"]["platform"], v["attributes"]["versionString"]) for v in items]
        raise SystemExit(f"!! expected exactly 1 {platform} appStoreVersion {version_string!r}, found {len(matches)}: {found}")
    return matches[0]


# ---------------------------------------------------------------------------
# 1. apps.contentRightsDeclaration
# ---------------------------------------------------------------------------


def ensure_content_rights(app: dict, desired: str) -> None:
    current = app["attributes"].get("contentRightsDeclaration")
    if current == desired:
        print(f"OK     apps.contentRightsDeclaration already {desired!r}")
        return
    status, body = request(
        "PATCH",
        f"/apps/{app['id']}",
        body={"data": {"type": "apps", "id": app["id"], "attributes": {"contentRightsDeclaration": desired}}},
    )
    if status != 200:
        fatal("PATCH apps.contentRightsDeclaration", status, body)
    print(f"SET    apps.contentRightsDeclaration: {current!r} -> {desired!r}")


# ---------------------------------------------------------------------------
# 2. appInfos primary/secondary category
# ---------------------------------------------------------------------------


def ensure_categories(app_info_id: str, primary: str, secondary: str) -> None:
    status, body = request("GET", f"/appInfos/{app_info_id}/primaryCategory")
    if status != 200:
        fatal("GET appInfos/primaryCategory", status, body)
    current_primary = (body.get("data") or {}).get("id")

    status, body = request("GET", f"/appInfos/{app_info_id}/secondaryCategory")
    if status != 200:
        fatal("GET appInfos/secondaryCategory", status, body)
    current_secondary = (body.get("data") or {}).get("id")

    if current_primary == primary and current_secondary == secondary:
        print(f"OK     appInfos categories already primary={primary} secondary={secondary}")
        return

    rels = {}
    if current_primary != primary:
        rels["primaryCategory"] = {"data": {"type": "appCategories", "id": primary}}
    if current_secondary != secondary:
        rels["secondaryCategory"] = {"data": {"type": "appCategories", "id": secondary}}

    status, body = request(
        "PATCH",
        f"/appInfos/{app_info_id}",
        body={"data": {"type": "appInfos", "id": app_info_id, "relationships": rels}},
    )
    if status != 200:
        fatal("PATCH appInfos categories", status, body)
    print(f"SET    appInfos.primaryCategory: {current_primary!r} -> {primary!r}")
    print(f"SET    appInfos.secondaryCategory: {current_secondary!r} -> {secondary!r}")


# ---------------------------------------------------------------------------
# 3. appInfoLocalizations (name / subtitle only -- privacyPolicyUrl deferred)
# ---------------------------------------------------------------------------


def ensure_app_info_localizations(app_info_id: str, locales: dict) -> None:
    existing = get_all_pages(f"/appInfos/{app_info_id}/appInfoLocalizations", params={"limit": 50})
    by_locale = {item["attributes"]["locale"]: item for item in existing}

    for locale, desired in locales.items():
        # privacyPolicyUrl intentionally NOT set: the URL in metadata.json is not live yet.
        desired_attrs = {"name": desired["name"], "subtitle": desired["subtitle"]}

        try:
            if locale in by_locale:
                item = by_locale[locale]
                changed = diff_attrs(item["attributes"], desired_attrs)
                if not changed:
                    print(f"OK     appInfoLocalizations[{locale}] already up to date")
                    continue
                status, body = request(
                    "PATCH",
                    f"/appInfoLocalizations/{item['id']}",
                    body={"data": {"type": "appInfoLocalizations", "id": item["id"], "attributes": changed}},
                )
                if status != 200:
                    fatal(f"PATCH appInfoLocalizations[{locale}]", status, body)
                print(f"SET    appInfoLocalizations[{locale}]: {changed}")
            else:
                body_attrs = dict(desired_attrs)
                body_attrs["locale"] = locale
                status, body = request(
                    "POST",
                    "/appInfoLocalizations",
                    body={
                        "data": {
                            "type": "appInfoLocalizations",
                            "attributes": body_attrs,
                            "relationships": {"appInfo": {"data": {"type": "appInfos", "id": app_info_id}}},
                        }
                    },
                )
                if status not in (200, 201):
                    fatal(f"POST appInfoLocalizations[{locale}]", status, body)
                print(f"CREATE appInfoLocalizations[{locale}]: {body_attrs}")
        except ApiError as e:
            print(f"!! appInfoLocalizations[{locale}]: {e}")
            FAILURES.append(f"appInfoLocalizations[{locale}]")


# ---------------------------------------------------------------------------
# 4. appStoreVersions.copyright / releaseType
# ---------------------------------------------------------------------------


def ensure_version_attrs(version: dict, copyright_: str, release_type: str) -> None:
    desired = {"copyright": copyright_, "releaseType": release_type}
    changed = diff_attrs(version["attributes"], desired)
    if not changed:
        print(f"OK     appStoreVersions already copyright={copyright_!r} releaseType={release_type!r}")
        return
    status, body = request(
        "PATCH",
        f"/appStoreVersions/{version['id']}",
        body={"data": {"type": "appStoreVersions", "id": version["id"], "attributes": changed}},
    )
    if status != 200:
        fatal("PATCH appStoreVersions", status, body)
    print(f"SET    appStoreVersions: {changed}")


# ---------------------------------------------------------------------------
# 5. appStoreVersionLocalizations (whatsNew intentionally left untouched/null)
# ---------------------------------------------------------------------------


def ensure_version_localizations(version_id: str, locales: dict) -> None:
    existing = get_all_pages(f"/appStoreVersions/{version_id}/appStoreVersionLocalizations", params={"limit": 50})
    by_locale = {item["attributes"]["locale"]: item for item in existing}

    for locale, desired in locales.items():
        desired_attrs = {
            "description": desired["description"],
            "keywords": desired["keywords"],
            "supportUrl": desired["supportUrl"],
            "marketingUrl": desired["marketingUrl"],
            "promotionalText": desired["promotionalText"],
        }
        # whatsNew must stay null for a 1.0 submission -- never sent.

        try:
            if locale in by_locale:
                item = by_locale[locale]
                changed = diff_attrs(item["attributes"], desired_attrs)
                if not changed:
                    print(f"OK     appStoreVersionLocalizations[{locale}] already up to date")
                    continue
                status, body = request(
                    "PATCH",
                    f"/appStoreVersionLocalizations/{item['id']}",
                    body={"data": {"type": "appStoreVersionLocalizations", "id": item["id"], "attributes": changed}},
                )
                if status != 200:
                    fatal(f"PATCH appStoreVersionLocalizations[{locale}]", status, body)
                print(f"SET    appStoreVersionLocalizations[{locale}]: keys={sorted(changed.keys())}")
            else:
                body_attrs = dict(desired_attrs)
                body_attrs["locale"] = locale
                status, body = request(
                    "POST",
                    "/appStoreVersionLocalizations",
                    body={
                        "data": {
                            "type": "appStoreVersionLocalizations",
                            "attributes": body_attrs,
                            "relationships": {"appStoreVersion": {"data": {"type": "appStoreVersions", "id": version_id}}},
                        }
                    },
                )
                if status not in (200, 201):
                    fatal(f"POST appStoreVersionLocalizations[{locale}]", status, body)
                print(f"CREATE appStoreVersionLocalizations[{locale}]: keys={sorted(body_attrs.keys())}")
        except ApiError as e:
            print(f"!! appStoreVersionLocalizations[{locale}]: {e}")
            FAILURES.append(f"appStoreVersionLocalizations[{locale}]")


# ---------------------------------------------------------------------------
# 6. ageRatingDeclarations (id == appInfo id)
# ---------------------------------------------------------------------------


def ensure_age_rating(app_info_id: str, desired_from_metadata: dict) -> None:
    desired = dict(desired_from_metadata)
    desired.update(AGE_RATING_EXTRA_FIELDS)

    # NOTE: GET /v1/ageRatingDeclarations/{id} 403s ("Allowed operation is:
    # UPDATE") -- the direct collection route only allows PATCH. Reading
    # current state must go through the appInfos-nested route instead.
    status, body = request("GET", f"/appInfos/{app_info_id}/ageRatingDeclaration")
    if status != 200:
        fatal("GET appInfos/ageRatingDeclaration", status, body)
    current = body["data"]["attributes"]

    changed = diff_attrs(current, desired)
    if not changed:
        print("OK     ageRatingDeclarations already matches desired state")
    else:
        status, body = request(
            "PATCH",
            f"/ageRatingDeclarations/{app_info_id}",
            body={"data": {"type": "ageRatingDeclarations", "id": app_info_id, "attributes": changed}},
        )
        if status != 200:
            fatal("PATCH ageRatingDeclarations", status, body)
        print(f"SET    ageRatingDeclarations: {sorted(changed.keys())}")

    status, body = request("GET", f"/appInfos/{app_info_id}")
    if status != 200:
        fatal("GET appInfos (re-check appStoreAgeRating)", status, body)
    rating = body["data"]["attributes"]["appStoreAgeRating"]
    print(f"       -> appStoreAgeRating = {rating}")
    if rating != "FOUR_PLUS":
        print(f"       !! WARNING: expected FOUR_PLUS, got {rating!r}")


# ---------------------------------------------------------------------------
# 7. appStoreReviewDetails (create-or-update)
# ---------------------------------------------------------------------------


def ensure_review_detail(version_id: str, details: dict) -> None:
    status, body = request("GET", f"/appStoreVersions/{version_id}/appStoreReviewDetail")
    if status != 200:
        fatal("GET appStoreReviewDetail", status, body)
    existing = body.get("data")

    desired_attrs = {
        "contactFirstName": details["contactFirstName"],
        "contactLastName": details["contactLastName"],
        "contactPhone": details["contactPhone"],
        "contactEmail": details["contactEmail"],
        "demoAccountRequired": details["demoAccountRequired"],
        "notes": details["notes"],
    }

    if existing:
        changed = diff_attrs(existing["attributes"], desired_attrs)
        if not changed:
            print("OK     appStoreReviewDetails already up to date")
            return
        status, body = request(
            "PATCH",
            f"/appStoreReviewDetails/{existing['id']}",
            body={"data": {"type": "appStoreReviewDetails", "id": existing["id"], "attributes": changed}},
        )
        if status != 200:
            fatal("PATCH appStoreReviewDetails", status, body)
        print(f"SET    appStoreReviewDetails: {sorted(changed.keys())}")
    else:
        status, body = request(
            "POST",
            "/appStoreReviewDetails",
            body={
                "data": {
                    "type": "appStoreReviewDetails",
                    "attributes": desired_attrs,
                    "relationships": {"appStoreVersion": {"data": {"type": "appStoreVersions", "id": version_id}}},
                }
            },
        )
        if status not in (200, 201):
            fatal("POST appStoreReviewDetails", status, body)
        print(f"CREATE appStoreReviewDetails: id={body['data']['id']}")


# ---------------------------------------------------------------------------
# 8. Price = free. Base territory USA + one manual price point (tier 0).
#    NOTE: appAvailabilityV2 / territory availability is deliberately never
#    touched by this script (see module docstring / final report).
# ---------------------------------------------------------------------------


def find_free_price_point(app_id: str) -> str:
    items = get_all_pages(f"/apps/{app_id}/appPricePoints", params={"filter[territory]": "USA", "limit": 200})
    for p in items:
        try:
            if float(p["attributes"]["customerPrice"]) == 0.0:
                return p["id"]
        except (TypeError, ValueError):
            continue
    raise ApiError("could not find a USA appPricePoint with customerPrice 0.0")


def ensure_free_price(app_id: str) -> None:
    status, body = request("GET", f"/appPriceSchedules/{app_id}/baseTerritory")
    base_territory = (body.get("data") or {}).get("id") if status == 200 else None

    # This relationship 404s with a generic NOT_FOUND when the collection is
    # empty (an observed ASC API quirk) rather than returning `"data": []`,
    # so a 404 here is treated as "no manual price set yet", not a hard error.
    status, body = request("GET", f"/appPriceSchedules/{app_id}/relationships/manualPrices")
    if status == 404:
        manual_prices: list = []
    elif status == 200:
        manual_prices = body.get("data") or []
    else:
        fatal("GET appPriceSchedules/relationships/manualPrices", status, body)

    if manual_prices:
        print(f"OK     appPriceSchedule already has manualPrices set: {manual_prices} (baseTerritory={base_territory}) -- leaving unchanged")
        return

    # manualPrices does NOT take appPricePoints ids directly (confirmed live:
    # ENTITY_ERROR.RELATIONSHIP.INVALID "type 'appPricePoints' is not valid
    # for the relationship 'manualPrices'"). It's a compound-document create:
    # manualPrices references an inline "appPrices" object (by a client-local
    # "${...}"-format id, per ENTITY_ERROR.INCLUDED.INVALID_ID's message) put
    # in "included", and that appPrices object's own relationships.appPricePoint
    # is what actually points at the real appPricePoints id.
    free_id = find_free_price_point(app_id)
    local_id = "${manual-price-free}"
    status, body = request(
        "POST",
        "/appPriceSchedules",
        body={
            "data": {
                "type": "appPriceSchedules",
                "relationships": {
                    "app": {"data": {"type": "apps", "id": app_id}},
                    "baseTerritory": {"data": {"type": "territories", "id": "USA"}},
                    "manualPrices": {"data": [{"type": "appPrices", "id": local_id}]},
                },
            },
            "included": [
                {
                    "type": "appPrices",
                    "id": local_id,
                    "relationships": {"appPricePoint": {"data": {"type": "appPricePoints", "id": free_id}}},
                }
            ],
        },
    )
    if status not in (200, 201):
        fatal("POST appPriceSchedules", status, body)
    print(f"CREATE appPriceSchedule: baseTerritory=USA manualPrices=[{free_id}] (free, previous baseTerritory was {base_territory!r})")


# ---------------------------------------------------------------------------
# Verification pass -- re-GETs everything and prints a concise summary.
# ---------------------------------------------------------------------------


def verify_all(app_id: str, app_info_id: str, version_id: str) -> None:
    print("\n" + "=" * 22 + " VERIFICATION (fresh GETs) " + "=" * 22)

    status, body = request("GET", f"/apps/{app_id}")
    a = body["data"]["attributes"]
    print(f"apps/{app_id}: contentRightsDeclaration={a['contentRightsDeclaration']!r}")

    status, body = request("GET", f"/appInfos/{app_info_id}/primaryCategory")
    pc = (body.get("data") or {}).get("id")
    status, body = request("GET", f"/appInfos/{app_info_id}/secondaryCategory")
    sc = (body.get("data") or {}).get("id")
    status, body = request("GET", f"/appInfos/{app_info_id}")
    rating = body["data"]["attributes"]["appStoreAgeRating"]
    print(f"appInfos/{app_info_id}: primaryCategory={pc} secondaryCategory={sc} appStoreAgeRating={rating}")

    for loc in get_all_pages(f"/appInfos/{app_info_id}/appInfoLocalizations", params={"limit": 50}):
        at = loc["attributes"]
        print(f"  appInfoLocalizations[{at['locale']}]: name={at['name']!r} subtitle={at['subtitle']!r} privacyPolicyUrl={at['privacyPolicyUrl']!r}")

    status, body = request("GET", f"/appStoreVersions/{version_id}")
    v = body["data"]["attributes"]
    print(f"appStoreVersions/{version_id}: copyright={v['copyright']!r} releaseType={v['releaseType']!r} state={v['appStoreState']!r}")

    for loc in get_all_pages(f"/appStoreVersions/{version_id}/appStoreVersionLocalizations", params={"limit": 50}):
        at = loc["attributes"]
        desc = (at["description"] or "")
        print(f"  appStoreVersionLocalizations[{at['locale']}]: keywords={at['keywords']!r}")
        print(f"    description ({len(desc)} chars): {desc[:70]!r}...")
        print(f"    supportUrl={at['supportUrl']!r} marketingUrl={at['marketingUrl']!r}")
        print(f"    promotionalText={at['promotionalText']!r} whatsNew={at['whatsNew']!r}")

    status, body = request("GET", f"/appStoreVersions/{version_id}/appStoreReviewDetail")
    rd = body.get("data")
    if rd:
        ra = rd["attributes"]
        print(
            f"appStoreReviewDetails: {ra['contactFirstName']} {ra['contactLastName']} / "
            f"{ra['contactPhone']} / {ra['contactEmail']} / demoAccountRequired={ra['demoAccountRequired']}"
        )
    else:
        print("appStoreReviewDetails: NOT SET")

    status, body = request("GET", f"/appPriceSchedules/{app_id}/baseTerritory")
    bt = (body.get("data") or {}).get("id") if status == 200 else None
    status, body = request("GET", f"/appPriceSchedules/{app_id}/relationships/manualPrices")
    mp = (body.get("data") or []) if status == 200 else []
    print(f"appPriceSchedule: baseTerritory={bt} manualPrices={mp}")

    status, body = request("GET", f"/apps/{app_id}/appAvailabilityV2")
    print(f"appAvailabilityV2: HTTP {status} (404/no data expected -- territory availability intentionally left unset)")


def main() -> None:
    metadata = load_metadata()
    app_id = metadata["appId"]

    app = get_app(app_id)
    if app["attributes"]["bundleId"] != BUNDLE_ID_IDENTIFIER:
        raise SystemExit(
            f"!! SAFETY ABORT: /apps/{app_id} has bundleId {app['attributes']['bundleId']!r}, "
            f"expected {BUNDLE_ID_IDENTIFIER!r}. Refusing to modify an app that doesn't match "
            f"asc_common.BUNDLE_ID_IDENTIFIER."
        )
    print(f"== app {app_id}: {app['attributes']['name']!r} ({app['attributes']['bundleId']}) ==")

    app_info = get_single_app_info(app_id)
    print(f"== appInfo {app_info['id']} ==")

    version = get_single_version(app_id, metadata["platform"], metadata["versionString"])
    print(f"== appStoreVersion {version['id']} ({version['attributes']['platform']} {version['attributes']['versionString']}) ==")

    print("\n--- 1. apps.contentRightsDeclaration ---")
    run_step("apps.contentRightsDeclaration", ensure_content_rights, app, metadata["contentRightsDeclaration"])

    print("\n--- 2. appInfos categories ---")
    run_step("appInfos.categories", ensure_categories, app_info["id"], metadata["primaryCategory"], metadata["secondaryCategory"])

    print("\n--- 3. appInfoLocalizations ---")
    ensure_app_info_localizations(app_info["id"], metadata["appInfoLocalizations"])

    print("\n--- 4. appStoreVersions copyright/releaseType ---")
    run_step("appStoreVersions.attrs", ensure_version_attrs, version, metadata["copyright"], "AFTER_APPROVAL")

    print("\n--- 5. appStoreVersionLocalizations ---")
    ensure_version_localizations(version["id"], metadata["versionLocalizations"])

    print("\n--- 6. ageRatingDeclarations ---")
    run_step("ageRatingDeclarations", ensure_age_rating, app_info["id"], metadata["ageRating"])

    print("\n--- 7. appStoreReviewDetails ---")
    run_step("appStoreReviewDetails", ensure_review_detail, version["id"], metadata["reviewDetails"])

    print("\n--- 8. price schedule (free) ---")
    run_step("appPriceSchedule", ensure_free_price, app_id)

    verify_all(app_id, app_info["id"], version["id"])

    print("\n== DEFERRED (intentionally not touched by this script) ==")
    print("  - privacyPolicyUrl on appInfoLocalizations: URL not live yet")
    print("  - App Privacy nutrition label: no public API for this (ASC web UI only)")
    print("  - screenshots / build attachment")
    print("  - appAvailabilityV2 / territory availability: left empty on purpose")

    if FAILURES:
        print("\n" + "=" * 24 + " FAILURES " + "=" * 24)
        for f in FAILURES:
            print(f"  - {f}")
        print(f"\n{len(FAILURES)} step(s) failed -- see full error bodies above.")
        sys.exit(1)

    print("\nDONE. All steps OK.")


if __name__ == "__main__":
    main()
