import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type Block,
  cutBlocks,
  type Inline,
  parseMarkdown,
} from "./markdown.ts";

function inline(nodes: readonly Inline[]): string {
  return nodes.map(inlineShape).join("");
}
function inlineShape(node: Inline): string {
  switch (node.kind) {
    case "text":
      return node.text;
    case "strong":
      return `<b>${inline(node.children)}</b>`;
    case "em":
      return `<i>${inline(node.children)}</i>`;
    case "code":
      return `<code>${node.text}</code>`;
    case "link":
      return `<a ${node.href}>${inline(node.children)}</a>`;
  }
}
type Shape = string | Shape[];
function shape(blocks: readonly Block[]): Shape[] {
  return blocks.map(blockShape);
}
function blockShape(block: Block): Shape {
  switch (block.kind) {
    case "paragraph":
      return ["p", inline(block.inlines)];
    case "heading":
      return [`h${block.level}`, inline(block.inlines)];
    case "code":
      return [
        `code:${block.lang}${block.closed ? "" : ":open"}`,
        block.lines.map((line) => line.text).join("\n"),
      ];
    case "list":
      return [
        block.ordered ? `ol:${block.start}` : "ul",
        block.items.map((item) => shape(item.blocks)),
      ];
    case "quote":
      return ["quote", shape(block.blocks)];
    case "table":
      return [
        `table:${block.align.join(",")}`,
        block.head.cells.map((cell) => inline(cell.inlines)),
        block.rows.map((row) => row.cells.map((cell) => inline(cell.inlines))),
      ];
  }
}
const md = (source: string) => shape(parseMarkdown(source));
type Leaf = Extract<Inline, { kind: "text" | "code" }>;
function texts(blocks: readonly Block[]): Leaf[] {
  const out: Leaf[] = [];
  const walk = (nodes: readonly Inline[]) => {
    for (const node of nodes) {
      if (node.kind === "text" || node.kind === "code") out.push(node);
      else walk(node.children);
    }
  };
  for (const block of blocks) {
    if (block.kind === "paragraph" || block.kind === "heading")
      walk(block.inlines);
    else if (block.kind === "list")
      for (const item of block.items) out.push(...texts(item.blocks));
    else if (block.kind === "quote") out.push(...texts(block.blocks));
    else if (block.kind === "table")
      for (const row of [block.head, ...block.rows])
        for (const cell of row.cells) walk(cell.inlines);
  }
  return out;
}
function tables(blocks: readonly Block[]): Extract<Block, { kind: "table" }>[] {
  return blocks.flatMap((block) =>
    block.kind === "table"
      ? [block]
      : block.kind === "list"
        ? block.items.flatMap((item) => tables(item.blocks))
        : block.kind === "quote"
          ? tables(block.blocks)
          : [],
  );
}
function cellCount(blocks: readonly Block[]): number {
  return tables(blocks).reduce(
    (sum, table) =>
      sum +
      table.head.cells.length +
      table.rows.reduce((count, row) => count + row.cells.length, 0),
    0,
  );
}
function depth(blocks: readonly Block[]): number {
  let deepest = 0;
  for (const block of blocks) {
    const inner =
      block.kind === "list"
        ? Math.max(0, ...block.items.map((item) => depth(item.blocks)))
        : block.kind === "quote"
          ? depth(block.blocks)
          : -1;
    deepest = Math.max(deepest, inner + 1);
  }
  return deepest;
}
function timed<T>(run: () => T): T {
  const started = performance.now();
  const value = run();
  const spent = performance.now() - started;
  assert.ok(spent < 2000, `parsing took ${spent}ms`);
  return value;
}

test("plain text stays one flow of paragraphs that rebuilds the source exactly", () => {
  const source =
    "  Hello there,\nsecond line keeps its break.\n\n\nThird paragraph after two blank lines: snake_case_name, 2 * 3 * 4, src/*.ts, a_b and \\@codex.\nSee https://example.com/docs.\n";
  const blocks = parseMarkdown(source);
  assert.deepEqual(
    blocks.map((block) => block.kind),
    ["paragraph", "paragraph"],
  );
  let rebuilt = "";
  let at = 0;
  for (const block of blocks) {
    assert.equal(block.kind, "paragraph");
    rebuilt +=
      source.slice(at, block.from) +
      inline(block.inlines).replace(/<a [^>]*>|<\/a>/g, "");
    at = block.to;
  }
  rebuilt += source.slice(at);
  assert.equal(rebuilt, source);
  assert.deepEqual(md("a_b_c and 2 * 3 * 4 and *.ts"), [
    ["p", "a_b_c and 2 * 3 * 4 and *.ts"],
  ]);
  assert.deepEqual(md("**/*.ts and snake_case_name_"), [
    ["p", "**/*.ts and snake_case_name_"],
  ]);
});

test("every text node is the slice of source it came from", () => {
  const source =
    "# Plan\n\nIntro **bold _both_** and `code` with [a link](https://a.test/x).\n\n- one\n  - nested *em*\n    continued\n- two\n\n> quoted\n> > deeper\n\n| a | b |\n|---|:-:|\n| c | d |\n";
  for (const node of texts(parseMarkdown(source))) {
    if (node.text === "\n")
      assert.equal(source.slice(node.from, node.to), "\n");
    else assert.equal(source.slice(node.from, node.to), node.text);
  }
});

test("emphasis, strong and inline code", () => {
  assert.deepEqual(md("**b** __b__ *i* _i_ ***bi*** `a*b*c` `` x`y ``"), [
    [
      "p",
      "<b>b</b> <b>b</b> <i>i</i> <i>i</i> <i><b>bi</b></i> <code>a*b*c</code> <code>x`y</code>",
    ],
  ]);
  assert.deepEqual(md("**bold with *em* inside**"), [
    ["p", "<b>bold with <i>em</i> inside</b>"],
  ]);
  assert.deepEqual(md("**across\nlines**"), [["p", "<b>across\nlines</b>"]]);
  assert.deepEqual(md("`https://example.com` and `\\@name`"), [
    ["p", "<code>https://example.com</code> and <code>\\@name</code>"],
  ]);
  assert.deepEqual(md("unclosed `tick and **star"), [
    ["p", "unclosed `tick and **star"],
  ]);
  assert.deepEqual(md("\\*not em\\* and \\`not code\\`"), [
    ["p", "*not em* and `not code`"],
  ]);
});

test("fenced code keeps its text, language and escapes", () => {
  assert.deepEqual(
    md("Run:\n```sh\nnpm test **x** \\@codex\n<b>raw</b>\n```\nDone."),
    [
      ["p", "Run:"],
      ["code:sh", "npm test **x** \\@codex\n<b>raw</b>"],
      ["p", "Done."],
    ],
  );
  assert.deepEqual(md("~~~\na\n~~~"), [["code:", "a"]]);
  assert.deepEqual(md("```\nnever closed\n\n# not a heading"), [
    ["code::open", "never closed\n\n# not a heading"],
  ]);
});

test("lists, ordered starts, nesting and lazy lines", () => {
  assert.deepEqual(md("Changes:\n- one\n- two\n\nAfter."), [
    ["p", "Changes:"],
    ["ul", [[["p", "one"]], [["p", "two"]]]],
    ["p", "After."],
  ]);
  assert.deepEqual(md("3. three\n4. four"), [
    ["ol:3", [[["p", "three"]], [["p", "four"]]]],
  ]);
  assert.deepEqual(md("- a\n  - b\n    - c\n- d"), [
    [
      "ul",
      [
        [
          ["p", "a"],
          [
            "ul",
            [
              [
                ["p", "b"],
                ["ul", [[["p", "c"]]]],
              ],
            ],
          ],
        ],
        [["p", "d"]],
      ],
    ],
  ]);
  assert.deepEqual(md("1. step\n    ```js\n    x()\n    ```\n2. next"), [
    [
      "ol:1",
      [
        [
          ["p", "step"],
          ["code:js", "x()"],
        ],
        [["p", "next"]],
      ],
    ],
  ]);
  assert.deepEqual(md("* a\nlazy line\n* b"), [
    ["ul", [[["p", "a\nlazy line"]], [["p", "b"]]]],
  ]);
  assert.deepEqual(md("- a\n\n- b"), [["ul", [[["p", "a"]], [["p", "b"]]]]]);
  assert.deepEqual(md("- a\n1. b"), [
    ["ul", [[["p", "a"]]]],
    ["ol:1", [[["p", "b"]]]],
  ]);
  assert.deepEqual(md("The year\n1999. was fine"), [
    ["p", "The year\n1999. was fine"],
  ]);
  assert.deepEqual(md("-5 degrees\n+1 from me"), [
    ["p", "-5 degrees\n+1 from me"],
  ]);
});

test("headings and block quotes", () => {
  assert.deepEqual(md("# One\n### Three ###\n#tag\n####### seven"), [
    ["h1", "One"],
    ["h3", "Three"],
    ["p", "#tag\n####### seven"],
  ]);
  assert.deepEqual(md("> a\n> b\n>\n> c\n> > deeper"), [
    [
      "quote",
      [
        ["p", "a\nb"],
        ["p", "c"],
        ["quote", [["p", "deeper"]]],
      ],
    ],
  ]);
});

test("tables with alignment, escaped pipes and ragged rows", () => {
  assert.deepEqual(
    md(
      "| Name | Count | Note |\n|:--|--:|:-:|\n| `a\\|b` | **2** |\n| c | 3 | x | extra |",
    ),
    [
      [
        "table:left,right,center",
        ["Name", "Count", "Note"],
        [
          ["<code>a|b</code>", "<b>2</b>"],
          ["c", "3", "x"],
        ],
      ],
    ],
  );
  assert.deepEqual(md("a | b\n\nTitle\n---"), [
    ["p", "a | b"],
    ["p", "Title\n---"],
  ]);
  assert.deepEqual(md("Intro\n| a | b |\n|---|---|\n| 1 | 2 |"), [
    ["p", "Intro"],
    ["table:,", ["a", "b"], [["1", "2"]]],
  ]);
  for (const rule of [
    "|---|---|",
    "| :-- | --: |",
    " |-|-| ",
    "|:-:|:-:|\r",
    "---|---",
    "\t-\t|\t-",
  ])
    assert.equal(parseMarkdown(`a|b\n${rule}`)[0]?.kind, "table", rule);
  const rejected: [string, string][] = [
    ["a", "|"],
    ["a|b|c", "|---||---|"],
    ["a|b", "|---|---\\|"],
    ["a|b", "| : | - |"],
    ["a|b", "|-- x|--|"],
    ["a|b", "|---|"],
  ];
  for (const [head, rule] of rejected)
    assert.equal(parseMarkdown(`${head}\n${rule}`)[0]?.kind, "paragraph", rule);
});

test("a table stops at 64 columns and 16384 cells, and never pads a row", () => {
  const row = (width: number) => `|${" x |".repeat(width)}`;
  const rule = (width: number) => `|${"-|".repeat(width)}`;
  assert.deepEqual(
    parseMarkdown(`${row(65)}\n${rule(65)}\n${row(65)}`).map((b) => b.kind),
    ["paragraph"],
  );
  const full = parseMarkdown(
    [row(64), rule(64), ...Array(300).fill(row(64))].join("\n"),
  );
  const [table, rest] = full;
  assert.equal(table?.kind, "table");
  assert.equal(rest?.kind, "paragraph");
  if (table?.kind !== "table") return;
  assert.equal(table.head.cells.length, 64);
  assert.equal(table.rows.length, 255);
  assert.equal(cellCount(full), 16_384);
  assert.deepEqual(md("| a | b | c |\n|-|-|-|\n|\n| 1 |\n| 1 | 2 | 3 | 4 |"), [
    ["table:,,", ["a", "b", "c"], [[""], ["1"], ["1", "2", "3"]]],
  ]);
});

test("the review's slow inputs parse in one bounded batch", () => {
  const W = 4000;
  const R = 8000;
  const bare = `${"|".repeat(W + 1)}\n${"|-".repeat(W)}|\n${"|\n".repeat(R)}`;
  const full = `|${" x |".repeat(64)}`;
  const inputs = [
    bare,
    [full, `|${"-|".repeat(64)}`, ...Array(R).fill(full)].join("\n"),
    `a|b\n${" ".repeat(80_000)}|x`,
    `a|b\n|-${" ".repeat(80_000)}x`,
    `https://a${".".repeat(80_000)}b`,
  ];
  const started = performance.now();
  const parsed = inputs.map((source) => parseMarkdown(source));
  const spent = performance.now() - started;
  assert.ok(spent < 300, `parsing took ${spent}ms`);
  for (const blocks of parsed) {
    for (const table of tables(blocks))
      assert.ok(table.head.cells.length <= 64, "column cap");
    assert.ok(cellCount(blocks) <= 16_384, "cell budget");
  }
  const [url] = parsed[4] ?? [];
  assert.equal(url?.kind, "paragraph");
  if (url?.kind !== "paragraph") return;
  assert.deepEqual(
    url.inlines.map((node) => node.kind),
    ["link"],
  );
});

test("links follow the http and https rule and never become images", () => {
  assert.deepEqual(md("[docs](https://a.test/d) [x](javascript:alert(1))"), [
    ["p", "<a https://a.test/d>docs</a> x"],
  ]);
  assert.deepEqual(
    md("![x](javascript:alert(1)) ![logo](https://a.test/l.png)"),
    [["p", "x <a https://a.test/l.png>logo</a>"]],
  );
  assert.deepEqual(md("[mail](mailto:a@b.c) [rel](./README.md)"), [
    ["p", "mail rel"],
  ]);
  assert.deepEqual(md("(see https://a.test/x). **https://a.test/b**"), [
    [
      "p",
      "(see <a https://a.test/x>https://a.test/x</a>). <b><a https://a.test/b>https://a.test/b</a></b>",
    ],
  ]);
  assert.deepEqual(md("[outer [inner](https://a.test/i)](https://a.test/o)"), [
    ["p", "<a https://a.test/o>outer [inner](https://a.test/i)</a>"],
  ]);
});

test("raw HTML stays literal text", () => {
  for (const source of [
    "<script>alert(1)</script>",
    "<img src=x onerror=alert(1)>",
    '<a href="javascript:alert(1)">x</a>',
  ])
    assert.deepEqual(md(source), [["p", source]]);
});

test("hostile nesting is capped and stays fast", () => {
  for (const source of [
    `${"- ".repeat(10_000)}x`,
    `${">".repeat(10_000)} x`,
    `${"> - ".repeat(5_000)}x`,
    Array.from({ length: 2_000 }, (_, at) => `${"  ".repeat(at)}- x`).join(
      "\n",
    ),
    `${"*a ".repeat(5_000)}${" a*".repeat(5_000)}`,
    `${"[".repeat(50_000)}${"](".repeat(50_000)}`,
    "`".repeat(100_000),
    "` ".repeat(50_000),
    "*a _b ".repeat(20_000),
  ]) {
    const blocks = timed(() => parseMarkdown(source));
    assert.ok(depth(blocks) <= 9, `depth ${depth(blocks)}`);
  }
});

test("a 500 by 30 table parses whole", () => {
  const row = (cell: string) =>
    `| ${Array.from({ length: 30 }, (_, at) => `${cell}${at}`).join(" | ")} |`;
  const source = [
    row("h"),
    `|${"---|".repeat(30)}`,
    ...Array.from({ length: 500 }, (_, at) => row(`r${at}c`)),
  ].join("\n");
  const [table] = timed(() => parseMarkdown(source));
  assert.equal(table?.kind, "table");
  if (table?.kind !== "table") return;
  assert.equal(table.head.cells.length, 30);
  assert.equal(table.rows.length, 500);
  assert.ok(table.rows.every((cells) => cells.cells.length === 30));
});

test("bare URLs drop trailing punctuation the same way at any length", () => {
  assert.deepEqual(md("https://a.test/x.). done"), [
    ["p", "<a https://a.test/x>https://a.test/x</a>.). done"],
  ]);
  assert.deepEqual(md("https://a.test/?q=1_*"), [
    ["p", "<a https://a.test/?q=1>https://a.test/?q=1</a>_*"],
  ]);
  assert.deepEqual(md("see https://a.test/x_, and https://b.test/y!"), [
    [
      "p",
      "see <a https://a.test/x>https://a.test/x</a>_, and <a https://b.test/y>https://b.test/y</a>!",
    ],
  ]);
});

test("cutting at a source offset keeps whole elements", () => {
  const fenced = "Intro\n```js\nconst a = 1;\nconst b = 2;\n```\nAfter";
  assert.deepEqual(
    shape(cutBlocks(parseMarkdown(fenced), fenced.indexOf("b ="))),
    [
      ["p", "Intro"],
      ["code:js:open", "const a = 1;\nconst "],
    ],
  );
  assert.deepEqual(
    shape(cutBlocks(parseMarkdown(fenced), fenced.indexOf("js"))),
    [["p", "Intro"]],
  );
  const list = "- one\n- two **bold** tail\n- three";
  assert.deepEqual(
    shape(cutBlocks(parseMarkdown(list), list.indexOf("ld**"))),
    [["ul", [[["p", "one"]], [["p", "two <b>bo</b>"]]]]],
  );
  const href = `https://a.test/${"y".repeat(50)}`;
  const url = `see ${href} now`;
  const [cut] = cutBlocks(parseMarkdown(url), 20);
  assert.deepEqual(shape(cut ? [cut] : []), [
    ["p", `see <a ${href}>${href.slice(0, 16)}</a>`],
  ]);
  const table = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
  assert.deepEqual(shape(cutBlocks(parseMarkdown(table), table.indexOf("4"))), [
    [
      "table:,",
      ["a", "b"],
      [
        ["1", "2"],
        ["3", ""],
      ],
    ],
  ]);
  assert.deepEqual(
    shape(cutBlocks(parseMarkdown(fenced), fenced.length)),
    shape(parseMarkdown(fenced)),
  );
});
