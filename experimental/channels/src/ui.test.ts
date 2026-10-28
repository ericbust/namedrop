import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import { initialState, reduce } from "./store.ts";
import { parseEvent } from "./wire.ts";

test("item 1: rendered display names never invent mentions and exact tokens stay separate", async () => {
  const vite = await createServer({
    server: { middlewareMode: true, hmr: false, ws: false },
  });
  try {
    const { Sidebar } = await vite.ssrLoadModule("/src/ui/Sidebar.tsx");
    const { MemberList } = await vite.ssrLoadModule("/src/ui/MemberList.tsx");
    const lines = (
      await readFile(
        new URL("./fixtures/real-frames.jsonl", import.meta.url),
        "utf8",
      )
    )
      .trim()
      .split("\n");
    for (const line of lines) assert.ok(parseEvent("space", line));
    const fixture = JSON.parse(lines[2] ?? "null");
    for (const [name, token] of [
      ["all", "actual-token"],
      ["My Project", null],
      ["testbed link-channels-nd1", "testbed-link-channels-nd1"],
    ] satisfies [string, string | null][]) {
      fixture.frame.space.display_name = name;
      fixture.frame.space.addressable_label = token;
      const event = parseEvent("space", JSON.stringify(fixture));
      assert.ok(event?.type === "space");
      const state = reduce(initialState(), event);
      const header = renderToStaticMarkup(
        createElement(MemberList, { channel: event.channel, name }),
      );
      assert.equal(
        header.match(/<h1[^>]*>(.*?)<\/h1>/)?.[1]?.replace(/<[^>]*>/g, ""),
        name,
      );
      const sidebar = renderToStaticMarkup(
        createElement(Sidebar, { state, onSelect() {} }),
      );
      assert.equal(
        sidebar
          .match(/<button[^>]*>(.*?)<\/button>/)?.[1]
          ?.replace(/<[^>]*>/g, ""),
        `${name}${token === null ? "Not addressable" : `#${token}`}`,
      );
      for (const markup of [header, sidebar])
        assert.ok(
          markup.includes(token === null ? "Not addressable" : `#${token}`),
          markup,
        );
    }
  } finally {
    await vite.close();
  }
});

test("item 6: paused and inactive channels with attention have no Needs you shortcuts, dots or summary", async () => {
  const vite = await createServer({
    server: { middlewareMode: true, hmr: false, ws: false },
  });
  try {
    const { Sidebar } = await vite.ssrLoadModule("/src/ui/Sidebar.tsx");
    const { MemberList } = await vite.ssrLoadModule("/src/ui/MemberList.tsx");
    const fixture = JSON.parse(
      (
        await readFile(
          new URL("./fixtures/real-frames.jsonl", import.meta.url),
          "utf8",
        )
      )
        .trim()
        .split("\n")[0] ?? "null",
    );
    fixture.frame.attention = [
      {
        source_kind: "coordination",
        summary: { text: "Approve this", truncated: false },
        opened_at_ms: 1,
      },
    ];
    for (const level of ["paused", "inactive", "needs_you"]) {
      fixture.frame.health =
        level === "needs_you" ? { level, items: 1 } : { level };
      const event = parseEvent("space", JSON.stringify(fixture));
      assert.ok(event?.type === "space");
      const state = reduce(initialState(), event);
      const sidebar = renderToStaticMarkup(
        createElement(Sidebar, { state, onSelect() {} }),
      );
      const header = renderToStaticMarkup(
        createElement(MemberList, { channel: event.channel, name: "alpha" }),
      );
      const needsYou = level === "needs_you";
      assert.equal(
        sidebar.includes(
          'aria-label="Needs you" class="sidebar-section attention-navigation"><h2>Needs you<span class="section-count">1',
        ),
        needsYou,
      );
      assert.equal(
        sidebar.includes('aria-label="Needs you"></span>'),
        needsYou,
      );
      assert.equal(header.includes('class="attention-summary"'), needsYou);
      assert.equal(header.includes("Approve this"), needsYou);
    }
  } finally {
    await vite.close();
  }
});
