"""Prove native discovery using only the isolated test installation."""

import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
import tomllib

root = Path(sys.argv[1]).resolve()
environment = dict(os.environ, HOME=str(root / "home"), CODEX_HOME=str(root / "codex"))
log = (root / "app-server.stderr").open("w")
server = subprocess.Popen(
    ["codex", "app-server"],
    env=environment,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=log,
    text=True,
)


def request(request_id, method, params):
    server.stdin.write(json.dumps(dict(id=request_id, method=method, params=params)) + "\n")
    server.stdin.flush()
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if not select.select([server.stdout], [], [], 1)[0]:
            continue
        line = server.stdout.readline()
        if not line:
            raise RuntimeError("Codex app-server closed")
        value = json.loads(line)
        if value.get("id") == request_id:
            if "error" in value:
                raise RuntimeError(value["error"])
            return value
    raise RuntimeError(f"{method} timed out")


try:
    request(
        1,
        "initialize",
        dict(
            clientInfo=dict(name="namedrop-owner-review", version="1"),
            capabilities=dict(experimentalApi=True),
        ),
    )
    server.stdin.write(json.dumps(dict(method="initialized")) + "\n")
    server.stdin.flush()
    result = request(2, "skills/list", dict(cwds=[str(root / "home")], forceReload=True))
    (root / "skills-list.json").write_text(json.dumps(result, indent=2))
    marketplaces = tomllib.loads((root / "codex/config.toml").read_text())["marketplaces"]
    expected = {"namedrop:namedrop"} | {
        "namedrop:" + name for name in marketplaces if name.startswith("namedrop-hc-")
    }
    skills = [
        skill
        for row in result["result"]["data"]
        for skill in row["skills"]
        if skill["name"].startswith("namedrop:")
    ]
    assert {skill["name"] for skill in skills} == expected, skills
    assert len(skills) == len(expected), skills
    for skill in skills:
        assert skill["enabled"], skill
        print(json.dumps({key: skill.get(key) for key in ["name", "path", "enabled", "pluginId"]}))

    result = request(3, "plugin/list", dict(cwds=[str(root / "home")], marketplaceKinds=["local"]))
    (root / "plugin-list.json").write_text(json.dumps(result, indent=2))
    assert not result["result"]["marketplaceLoadErrors"], result
    for index, marketplace in enumerate(result["result"]["marketplaces"]):
        read = request(
            10 + index,
            "plugin/read",
            dict(pluginName="namedrop", marketplacePath=marketplace["path"]),
        )
        (root / ("plugin-read-" + marketplace["name"] + ".json")).write_text(json.dumps(read, indent=2))
        plugin = read["result"]["plugin"]
        assert len(plugin["hooks"]) == 2, plugin
        for hook in plugin["hooks"]:
            assert hook["key"].startswith("namedrop@" + marketplace["name"] + ":"), hook
            print(hook["key"])
        if marketplace["name"].startswith("namedrop-hc-"):
            assert len(plugin["skills"]) == 1, plugin
finally:
    server.terminate()
    try:
        server.wait(timeout=5)
    except subprocess.TimeoutExpired:
        server.kill()
        server.wait()
    log.close()
