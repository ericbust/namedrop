import assert from "node:assert/strict";
import { test } from "node:test";
import type { Channel, Repo, SpaceKey } from "./model.ts";
import {
  emptyLayout,
  type Group,
  groupChannels,
  LAYOUT_KEY,
  type Layout,
  loadLayout,
  moveChannel,
  moveGroup,
  saveLayout,
  toggleCollapsed,
} from "./sidebar-layout.ts";
import { compareChannels } from "./store.ts";

const key = (n: number) => n.toString(16).padStart(20, "0") as SpaceKey;
const namedrop: Repo = { id: "r-nd", name: "namedrop" };
const herdr: Repo = { id: "r-hd", name: "herdr" };
function channel(
  n: number,
  displayName: string,
  repo: Repo | null,
  level: "calm" | "needs_you" = "calm",
): Channel {
  return {
    key: key(n),
    workspaceId: `w${n}`,
    displayName,
    repo,
    health: level === "needs_you" ? { level, items: 1 } : { level },
  } as Channel;
}
const sorted = (...channels: Channel[]) => channels.sort(compareChannels);
const shape = (groups: Group[]) =>
  groups.map((group) => [
    group.key,
    group.name,
    group.channels.map((each) => each.displayName),
  ]);
const none = new Set<SpaceKey>();

const workspaces = () =>
  sorted(
    channel(1, "namedrop", namedrop),
    channel(2, "exp-ui", namedrop),
    channel(3, "exp-dashboard", namedrop, "needs_you"),
    channel(4, "herdr", herdr),
    channel(5, "scratch", null),
  );

test("grouping: one group per repo by name, a single workspace still a group, no repo in Other last", () => {
  const groups = groupChannels(workspaces(), emptyLayout, new Set([key(4)]));
  assert.deepEqual(shape(groups), [
    ["repo:r-hd", "herdr", ["herdr"]],
    ["repo:r-nd", "namedrop", ["exp-dashboard", "exp-ui", "namedrop"]],
    ["other", "Other", ["scratch"]],
  ]);
  assert.deepEqual(
    groups.map((group) => [group.needsYou, group.unread]),
    [
      [0, true],
      [1, false],
      [0, false],
    ],
  );
});

test("merge: the saved order comes first, and a new group or channel joins at the end in default order", () => {
  const layout: Layout = {
    groups: ["repo:r-nd", "repo:r-hd"],
    channels: [key(1), key(2)],
    collapsed: [],
  };
  const groups = groupChannels(
    sorted(
      ...workspaces(),
      channel(6, "a-new", namedrop),
      channel(7, "zed", null),
    ),
    layout,
    none,
  );
  assert.deepEqual(shape(groups), [
    ["repo:r-nd", "namedrop", ["namedrop", "exp-ui", "a-new", "exp-dashboard"]],
    ["repo:r-hd", "herdr", ["herdr"]],
    ["other", "Other", ["scratch", "zed"]],
  ]);
});

test("merge: saved keys that are gone are skipped, and the rest keep their order", () => {
  const layout: Layout = {
    groups: ["repo:gone", "other", "repo:r-nd"],
    channels: [key(99), key(2), key(98), key(1)],
    collapsed: ["repo:gone"],
  };
  assert.deepEqual(shape(groupChannels(workspaces(), layout, none)), [
    ["other", "Other", ["scratch"]],
    ["repo:r-nd", "namedrop", ["exp-ui", "namedrop", "exp-dashboard"]],
    ["repo:r-hd", "herdr", ["herdr"]],
  ]);
});

test("merge: a renamed repo or workspace keeps its place, because the keys are the repo id and the space key", () => {
  const layout: Layout = {
    groups: ["repo:r-nd", "repo:r-hd"],
    channels: [key(2), key(1)],
    collapsed: [],
  };
  const renamed = sorted(
    channel(1, "namedrop", { id: "r-nd", name: "zz-renamed" }),
    channel(2, "zz-ui", { id: "r-nd", name: "zz-renamed" }),
    channel(4, "herdr", herdr),
  );
  assert.deepEqual(shape(groupChannels(renamed, layout, none)), [
    ["repo:r-nd", "zz-renamed", ["zz-ui", "namedrop"]],
    ["repo:r-hd", "herdr", ["herdr"]],
  ]);
});

test("merge: a workspace that moves to another repo joins that group in its saved place", () => {
  const layout: Layout = {
    groups: ["repo:r-hd", "repo:r-nd"],
    channels: [key(2), key(4), key(1)],
    collapsed: [],
  };
  const moved = sorted(
    channel(1, "namedrop", namedrop),
    channel(2, "exp-ui", herdr),
    channel(4, "herdr", herdr),
  );
  assert.deepEqual(shape(groupChannels(moved, layout, none)), [
    ["repo:r-hd", "herdr", ["exp-ui", "herdr"]],
    ["repo:r-nd", "namedrop", ["namedrop"]],
  ]);
});

test("moves: a group or channel moves to the index asked, clamped, and the write keeps only what is visible", () => {
  const start: Layout = {
    groups: ["repo:gone"],
    channels: [key(99)],
    collapsed: ["repo:r-hd"],
  };
  const visible = groupChannels(workspaces(), start, none);
  const groupMoved = moveGroup(start, visible, "other", 0);
  assert.deepEqual(groupMoved, {
    groups: ["other", "repo:r-hd", "repo:r-nd"],
    channels: [key(5), key(4), key(3), key(2), key(1)],
    collapsed: ["repo:r-hd"],
  });
  assert.deepEqual(
    shape(groupChannels(workspaces(), groupMoved, none)).map(([k]) => k),
    ["other", "repo:r-hd", "repo:r-nd"],
  );
  const last = moveGroup(start, visible, "repo:r-hd", 99);
  assert.deepEqual(last.groups, ["repo:r-nd", "other", "repo:r-hd"]);

  const channelMoved = moveChannel(start, visible, key(1), 0);
  assert.deepEqual(shape(groupChannels(workspaces(), channelMoved, none))[1], [
    "repo:r-nd",
    "namedrop",
    ["namedrop", "exp-dashboard", "exp-ui"],
  ]);
  assert.deepEqual(channelMoved.collapsed, ["repo:r-hd"]);
  assert.equal(moveGroup(start, visible, "repo:nope", 0), start);
});

test("moves: a move in the flat list keeps channel order but leaves Other last once repos appear", () => {
  const flat = sorted(
    channel(1, "alpha", null),
    channel(2, "beta", null),
    channel(3, "gamma", null),
  );
  const visible = groupChannels(flat, emptyLayout, none);
  const saved = moveChannel(emptyLayout, visible, key(3), 0);
  const later = sorted(
    channel(1, "alpha", namedrop),
    channel(2, "beta", null),
    channel(3, "gamma", null),
  );
  assert.deepEqual(shape(groupChannels(later, saved, none)), [
    ["repo:r-nd", "namedrop", ["alpha"]],
    ["other", "Other", ["gamma", "beta"]],
  ]);
});

test("collapse: toggling adds and removes the group and leaves the order alone", () => {
  const layout: Layout = {
    groups: ["repo:r-nd"],
    channels: [key(1)],
    collapsed: ["other"],
  };
  const collapsed = toggleCollapsed(layout, "repo:r-nd");
  assert.deepEqual(collapsed, { ...layout, collapsed: ["other", "repo:r-nd"] });
  assert.deepEqual(toggleCollapsed(collapsed, "repo:r-nd"), layout);
});

function memoryStorage(initial: string | null = null) {
  let value = initial;
  return {
    getItem: (name: string) => (name === LAYOUT_KEY ? value : null),
    setItem: (name: string, next: string) => {
      assert.equal(name, LAYOUT_KEY);
      value = next;
    },
  };
}

test("storage: a saved layout loads back the same", () => {
  const storage = memoryStorage();
  const layout: Layout = {
    groups: ["repo:r-nd", "other"],
    channels: [key(2), key(1)],
    collapsed: ["other"],
  };
  saveLayout(storage, layout);
  assert.deepEqual(loadLayout(storage), layout);
  assert.deepEqual(loadLayout(memoryStorage()), emptyLayout);
});

test("storage: corrupt, mistyped or unreadable storage loads as the empty layout, and a failed save is swallowed", () => {
  for (const raw of ["{", "null", "7", '"text"', "[]"])
    assert.deepEqual(loadLayout(memoryStorage(raw)), emptyLayout, raw);
  assert.deepEqual(
    loadLayout(
      memoryStorage(
        JSON.stringify({
          groups: ["repo:a", 3, "nonsense", "other", null],
          channels: "nope",
          collapsed: [{}, "repo:b"],
        }),
      ),
    ),
    { groups: ["repo:a", "other"], channels: [], collapsed: ["repo:b"] },
  );
  const throwing = {
    getItem: () => {
      throw new Error("SecurityError");
    },
    setItem: () => {
      throw new Error("QuotaExceededError");
    },
  };
  assert.deepEqual(loadLayout(throwing), emptyLayout);
  assert.doesNotThrow(() => saveLayout(throwing, emptyLayout));
});
