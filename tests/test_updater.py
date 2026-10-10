from quiekel_embed.updater import REPO_URL, is_newer, parse_release, parse_version


def release(tag="v0.3.0", **extra):
    return {"tag_name": tag, "html_url": f"{REPO_URL}/releases/tag/{tag}", "name": "Pigs fly",
            "body": "notes", "published_at": "2026-10-12T10:00:00Z", "draft": False,
            "prerelease": False, **extra}


def test_versions():
    assert parse_version("v1.2.3") == (1, 2, 3)
    assert parse_version("1.2") is None
    assert is_newer("v0.10.0", "0.9.9")
    assert not is_newer("v0.2.0", "0.2.0")
    assert not is_newer("garbage", "0.1.0")


def test_newer_final_release_is_offered():
    r = parse_release(release(), "0.2.0")
    assert r == {"version": "0.3.0", "tag": "v0.3.0", "title": "Pigs fly", "notes": "notes",
                 "published": "2026-10-12", "url": f"{REPO_URL}/releases/tag/v0.3.0"}


def test_untrusted_releases_are_ignored():
    assert parse_release(release(), "0.3.0") is None  # not newer
    assert parse_release(release(draft=True), "0.2.0") is None
    assert parse_release(release(prerelease=True), "0.2.0") is None
    # Tags end up in git commands: anything but vX.Y.Z is refused.
    assert parse_release(release(tag="v9.9.9 --upload-pack=evil"), "0.2.0") is None
    assert parse_release(release(tag="--force"), "0.2.0") is None
    # The link shown in the app must point at this repository.
    evil = release(html_url="https://evil.example/releases/v0.3.0")
    assert parse_release(evil, "0.2.0") is None
