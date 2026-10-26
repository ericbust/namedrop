"""Where a testbed gets Namedrop from, and how each kind of source is installed.

A source is a plain dict so it can be saved in state.json unchanged:

    {"kind": "ref", "repo": "ericbust/namedrop", "ref": "main"}
    {"kind": "pr", "repo": ..., "number": 27, "head_ref": ..., "head_repo": ..., "sha": ...}
    {"kind": "link", "path": "/abs/checkout", "commit": "<sha>"}
"""

import json
import re
import urllib.error
import urllib.request

DEFAULT_REPO = "ericbust/namedrop"
REPO_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


class SourceError(RuntimeError):
    pass


def slug(text):
    return re.sub(r"[^A-Za-z0-9._-]", "-", text)


def default_profile_name(request):
    """Name a profile from an unresolved request: the parts the user typed."""
    if request["kind"] == "ref":
        return "ref-" + slug(request["ref"])
    if request["kind"] == "pr":
        return "pr-{}".format(request["number"])
    return "link-" + slug(request["path"].rstrip("/").rsplit("/", 1)[-1])


def identity(source):
    """The fields that say which source this is, before and after resolution."""
    if source["kind"] == "ref":
        return ("ref", source["repo"].lower(), source["ref"])
    if source["kind"] == "pr":
        return ("pr", source["repo"].lower(), source["number"])
    return ("link", source["path"])


def describe(source):
    if source["kind"] == "ref":
        return "{} at {}".format(source["repo"], source["ref"])
    if source["kind"] == "pr":
        return "{} PR #{} ({}:{} at {})".format(source["repo"], source["number"], source["head_repo"],
                                               source["head_ref"], source["sha"][:7])
    return "local checkout {} at {}".format(source["path"], source["commit"][:7])


def parse_pull(repo, number, payload):
    head = payload["head"]
    # A fork deleted after the PR opened leaves head.repo null; pull/N/head still fetches.
    head_repo = (head.get("repo") or {}).get("full_name") or "(deleted fork)"
    return {"kind": "pr", "repo": repo, "number": number, "head_ref": head["ref"],
            "head_repo": head_repo, "sha": head["sha"], "state": payload.get("state", "unknown")}


def resolve_pull(repo, number, token=None, urlopen=urllib.request.urlopen):
    request = urllib.request.Request(
        "https://api.github.com/repos/{}/pulls/{}".format(repo, number),
        headers={"Accept": "application/vnd.github+json", "User-Agent": "namedrop-testbed",
                 "X-GitHub-Api-Version": "2022-11-28"})
    if token:
        request.add_header("Authorization", "Bearer " + token)
    try:
        with urlopen(request, timeout=30) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise SourceError("{} has no pull request #{}".format(repo, number))
        if error.code in (403, 429):
            raise SourceError("GitHub refused the request (HTTP {}), probably its rate limit. "
                              "Set GITHUB_TOKEN to a token and retry.".format(error.code))
        raise SourceError("GitHub returned HTTP {} for PR #{}".format(error.code, number))
    except (urllib.error.URLError, OSError) as error:
        raise SourceError("cannot reach GitHub to resolve PR #{}: {}".format(number, error))
    try:
        return parse_pull(repo, number, payload)
    except (KeyError, TypeError):
        raise SourceError("GitHub returned an unexpected payload for PR #{}".format(number))


def _pull_plan(source):
    if source["head_repo"].lower() == source["repo"].lower():
        return {"method": "install", "repo": source["repo"], "ref": source["head_ref"]}
    branch = "pr-{}".format(source["number"])
    return {"method": "link", "clone": "https://github.com/{}.git".format(source["repo"]),
            "fetch": "pull/{}/head:{}".format(source["number"], branch), "checkout": branch}


INSTALL_PLANS = {
    # The path users take: Herdr clones, builds and registers the plugin itself.
    "ref": lambda source: {"method": "install", "repo": source["repo"], "ref": source["ref"]},
    "pr": _pull_plan,
    "link": lambda source: {"method": "link", "clone": source["path"], "fetch": None,
                            "checkout": source["commit"]},
}


def install_plan(source):
    return INSTALL_PLANS[source["kind"]](source)
