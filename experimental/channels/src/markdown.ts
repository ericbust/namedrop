export type Range = { readonly from: number; readonly to: number };
export type Inline =
  | (Range & { readonly kind: "text"; readonly text: string })
  | (Range & { readonly kind: "strong"; readonly children: readonly Inline[] })
  | (Range & { readonly kind: "em"; readonly children: readonly Inline[] })
  | (Range & { readonly kind: "code"; readonly text: string })
  | (Range & {
      readonly kind: "link";
      readonly href: string;
      readonly children: readonly Inline[];
    });
export type CodeLine = Range & { readonly text: string };
export type ListItem = Range & { readonly blocks: readonly Block[] };
export type TableCell = Range & { readonly inlines: readonly Inline[] };
export type TableRow = Range & { readonly cells: readonly TableCell[] };
export type Align = "left" | "center" | "right" | null;
export type Block =
  | (Range & {
      readonly kind: "paragraph";
      readonly inlines: readonly Inline[];
    })
  | (Range & {
      readonly kind: "heading";
      readonly level: number;
      readonly inlines: readonly Inline[];
    })
  | (Range & {
      readonly kind: "code";
      readonly lang: string;
      readonly lines: readonly CodeLine[];
      readonly closed: boolean;
    })
  | (Range & {
      readonly kind: "list";
      readonly ordered: boolean;
      readonly start: number;
      readonly items: readonly ListItem[];
    })
  | (Range & { readonly kind: "quote"; readonly blocks: readonly Block[] })
  | (Range & {
      readonly kind: "table";
      readonly align: readonly Align[];
      readonly head: TableRow;
      readonly rows: readonly TableRow[];
    });

type Line = { readonly start: number; readonly end: number };
type Delim = Range & {
  readonly kind: "delim";
  readonly text: string;
  readonly open: boolean;
  readonly close: boolean;
};
type Token = Inline | Delim;

const MAX_DEPTH = 8;
const MAX_DESTINATION = 1024;
// A table wider than a timeline can show is text, and one table's cells stop
// at 64 columns by 256 rows so a message never grows past one screenful of DOM.
const MAX_COLUMNS = 64;
const MAX_CELLS = 16_384;
const FENCE = /^ {0,3}(`{3,}|~{3,})(.*)$/;
const QUOTE = /^ {0,3}> ?/;
const MARKER = /^( {0,3})([-+*]|\d{1,9}[.)])(?=[ \t]|$)/;
const DELIMITER_CELL = /^:?-+:?$/;
const BLANK = /^[ \t\r]*$/;
const BARE_URL = /https?:\/\/[^\s<>"']+/y;
const URL_TRAIL = new Set([".", ",", ";", "!", "?", ")", "*", "_"]);
const SAFE_HREF = /^https?:\/\//;
const ESCAPABLE = new Set(["*", "_", "`", "[", "]", "|", "#"]);
const SPACE = /\s/;
const PUNCTUATION = /[\p{P}\p{S}]/u;

export function parseMarkdown(source: string): Block[] {
  const text = (line: Line) => source.slice(line.start, line.end);
  const blank = (line: Line) => BLANK.test(text(line));
  const indent = (line: Line) => {
    let columns = 0;
    for (let at = line.start; at < line.end; at++) {
      if (source[at] === " ") columns++;
      else if (source[at] === "\t") columns += 4 - (columns % 4);
      else break;
    }
    return columns;
  };
  const strip = (line: Line, columns: number): Line => {
    let at = line.start;
    for (let taken = 0; at < line.end && taken < columns; at++) {
      if (source[at] === " ") taken++;
      else if (source[at] === "\t") taken += 4 - (taken % 4);
      else break;
    }
    return { start: at, end: line.end };
  };

  type Marker = {
    ordered: boolean;
    delimiter: string;
    start: number;
    width: number;
    content: Line;
    empty: boolean;
  };
  const marker = (line: Line): Marker | null => {
    const found = MARKER.exec(text(line));
    if (!found) return null;
    const [whole, , symbol = ""] = found;
    const rest = source.slice(line.start + whole.length, line.end);
    const empty = BLANK.test(rest);
    const spaces = /^[ \t]*/.exec(rest)?.[0].length ?? 0;
    const gap = empty || spaces > 4 ? 1 : spaces;
    return {
      ordered: /\d/.test(symbol),
      delimiter: symbol.at(-1) ?? "",
      start: Number.parseInt(symbol, 10) || 0,
      width: whole.length + gap,
      content: empty
        ? { start: line.end, end: line.end }
        : { start: line.start + whole.length + gap, end: line.end },
      empty,
    };
  };
  const sameList = (a: Marker, b: Marker) =>
    a.ordered === b.ordered && a.delimiter === b.delimiter;

  const fenceOpen = (line: Line) => {
    const found = FENCE.exec(text(line));
    if (!found) return null;
    const [, run = "", info = ""] = found;
    return run.startsWith("`") && info.includes("`") ? null : { run, info };
  };
  const fenceCloses = (line: Line, run: string) => {
    const value = text(line);
    let at = 0;
    while (at < 3 && value[at] === " ") at++;
    let length = 0;
    while (value[at + length] === run[0]) length++;
    return length >= run.length && BLANK.test(value.slice(at + length));
  };
  const headingAt = (line: Line) => {
    const value = text(line);
    let at = 0;
    while (at < 3 && value[at] === " ") at++;
    let level = 0;
    while (value[at + level] === "#") level++;
    const after = value[at + level];
    if (level < 1 || level > 6 || (after !== " " && after !== "\t"))
      return null;
    let start = at + level;
    while (value[start] === " " || value[start] === "\t") start++;
    let end = value.trimEnd().length;
    let closing = end;
    while (closing > start && value[closing - 1] === "#") closing--;
    if (
      closing === start ||
      value[closing - 1] === " " ||
      value[closing - 1] === "\t"
    )
      end = value.slice(0, closing).trimEnd().length;
    return end > start
      ? { level, content: { start: line.start + start, end: line.start + end } }
      : null;
  };
  const cells = (line: Line): Line[] => {
    const value = text(line);
    let start = value.length - value.trimStart().length;
    let end = value.trimEnd().length;
    if (value[start] === "|") start++;
    if (end > start && value[end - 1] === "|" && value[end - 2] !== "\\") end--;
    const out: Line[] = [];
    let cell = start;
    for (let at = start; at <= end; at++) {
      if (at < end && (value[at] !== "|" || value[at - 1] === "\\")) continue;
      const piece = value.slice(cell, at);
      const lead = piece.length - piece.trimStart().length;
      out.push({
        start: line.start + cell + lead,
        end: line.start + cell + Math.max(lead, piece.trimEnd().length),
      });
      cell = at + 1;
    }
    return out;
  };
  const delimiterRow = (line: Line) => {
    const pieces = cells(line);
    return pieces.every((piece) => DELIMITER_CELL.test(text(piece)));
  };
  const tableAt = (lines: readonly Line[], at: number) => {
    const head = lines[at];
    const rule = lines[at + 1];
    if (
      !head ||
      !rule ||
      !text(head).includes("|") ||
      !text(rule).includes("|") ||
      !delimiterRow(rule)
    )
      return null;
    const columns = cells(head);
    const rules = cells(rule);
    return columns.length === rules.length && columns.length <= MAX_COLUMNS
      ? { columns, rules }
      : null;
  };
  const interrupts = (lines: readonly Line[], at: number, depth: number) => {
    const line = lines[at];
    if (!line) return false;
    if (fenceOpen(line) || headingAt(line) || tableAt(lines, at)) return true;
    if (depth >= MAX_DEPTH) return false;
    if (QUOTE.test(text(line))) return true;
    const found = marker(line);
    return (
      found !== null && !found.empty && (!found.ordered || found.start === 1)
    );
  };
  const startsBlock = (lines: readonly Line[], at: number, depth: number) => {
    const line = lines[at];
    return (
      line !== undefined &&
      (marker(line) !== null || interrupts(lines, at, depth))
    );
  };

  const inlines = (pieces: readonly Line[], cell = false) =>
    parseInlines(source, pieces, cell);

  type Found = readonly [Block, number] | null;
  const fence = (lines: readonly Line[], at: number): Found => {
    const line = lines[at] as Line;
    const open = fenceOpen(line);
    if (!open) return null;
    const lang = open.info.trim().split(/\s+/)[0] ?? "";
    const offset = text(line).length - text(line).trimStart().length;
    const code: CodeLine[] = [];
    for (let next = at + 1; next < lines.length; next++) {
      const current = lines[next] as Line;
      if (fenceCloses(current, open.run))
        return [
          {
            kind: "code",
            lang,
            lines: code,
            closed: true,
            from: line.start,
            to: current.end,
          },
          next + 1,
        ];
      const content = strip(current, offset);
      code.push({ text: text(content), from: content.start, to: content.end });
    }
    return [
      {
        kind: "code",
        lang,
        lines: code,
        closed: false,
        from: line.start,
        to: lines.at(-1)?.end ?? line.end,
      },
      lines.length,
    ];
  };
  const heading = (lines: readonly Line[], at: number): Found => {
    const line = lines[at] as Line;
    const found = headingAt(line);
    return found
      ? [
          {
            kind: "heading",
            level: found.level,
            inlines: inlines([found.content]),
            from: line.start,
            to: line.end,
          },
          at + 1,
        ]
      : null;
  };
  const quote = (lines: readonly Line[], at: number, depth: number): Found => {
    const inner: Line[] = [];
    let next = at;
    for (; next < lines.length; next++) {
      const line = lines[next] as Line;
      const found = QUOTE.exec(text(line));
      if (!found) break;
      inner.push({ start: line.start + found[0].length, end: line.end });
    }
    if (inner.length === 0) return null;
    return [
      {
        kind: "quote",
        blocks: blocks(inner, depth + 1),
        from: (lines[at] as Line).start,
        to: (lines[next - 1] as Line).end,
      },
      next,
    ];
  };
  const list = (lines: readonly Line[], at: number, depth: number): Found => {
    const first = marker(lines[at] as Line);
    if (!first) return null;
    const items: ListItem[] = [];
    let next = at;
    while (next < lines.length) {
      const line = lines[next] as Line;
      const item = marker(line);
      if (!item || !sameList(first, item)) break;
      const content: Line[] = [item.content];
      let last = next;
      next++;
      while (next < lines.length) {
        const current = lines[next] as Line;
        if (blank(current)) {
          let after = next;
          while (after < lines.length && blank(lines[after] as Line)) after++;
          const resumed = lines[after];
          if (!resumed || indent(resumed) < item.width) break;
          for (; next < after; next++)
            content.push(strip(lines[next] as Line, item.width));
          continue;
        }
        if (indent(current) >= item.width) {
          content.push(strip(current, item.width));
        } else if (
          !blank(lines[next - 1] as Line) &&
          !startsBlock(lines, next, depth)
        ) {
          content.push(strip(current, indent(current)));
        } else break;
        last = next;
        next++;
      }
      items.push({
        blocks: blocks(content, depth + 1),
        from: line.start,
        to: (lines[last] as Line).end,
      });
      let after = next;
      while (after < lines.length && blank(lines[after] as Line)) after++;
      const sibling = lines[after] && marker(lines[after] as Line);
      if (after > next && sibling && sameList(first, sibling)) next = after;
    }
    return [
      {
        kind: "list",
        ordered: first.ordered,
        start: first.start,
        items,
        from: (lines[at] as Line).start,
        to: items.at(-1)?.to ?? (lines[at] as Line).end,
      },
      next,
    ];
  };
  const table = (lines: readonly Line[], at: number): Found => {
    const found = tableAt(lines, at);
    if (!found) return null;
    const width = found.columns.length;
    const row = (line: Line, pieces: readonly Line[]): TableRow => ({
      cells: pieces.slice(0, width).map((piece) => ({
        inlines: inlines([piece], true),
        from: piece.start,
        to: piece.end,
      })),
      from: line.start,
      to: line.end,
    });
    const head = lines[at] as Line;
    const rows: TableRow[] = [];
    let budget = MAX_CELLS - width;
    let next = at + 2;
    for (; next < lines.length; next++) {
      const line = lines[next] as Line;
      if (blank(line) || !text(line).includes("|")) break;
      const pieces = cells(line).slice(0, width);
      if (pieces.length > budget) break;
      budget -= pieces.length;
      rows.push(row(line, pieces));
    }
    return [
      {
        kind: "table",
        align: found.rules.map((rule) => {
          const value = source.slice(rule.start, rule.end);
          const left = value.startsWith(":");
          const right = value.endsWith(":");
          return left && right
            ? "center"
            : right
              ? "right"
              : left
                ? "left"
                : null;
        }),
        head: row(head, found.columns),
        rows,
        from: head.start,
        to: rows.at(-1)?.to ?? (lines[at + 1] as Line).end,
      },
      next,
    ];
  };
  const paragraph = (lines: readonly Line[], at: number, depth: number) => {
    const pieces = [lines[at] as Line];
    let next = at + 1;
    for (; next < lines.length; next++) {
      const line = lines[next] as Line;
      if (blank(line) || interrupts(lines, next, depth)) break;
      pieces.push(line);
    }
    const block: Block = {
      kind: "paragraph",
      inlines: inlines(pieces),
      from: pieces[0]?.start ?? 0,
      to: pieces.at(-1)?.end ?? 0,
    };
    return [block, next] as const;
  };
  const blocks = (lines: readonly Line[], depth: number): Block[] => {
    const out: Block[] = [];
    let at = 0;
    while (at < lines.length) {
      if (blank(lines[at] as Line)) {
        at++;
        continue;
      }
      const [block, next] =
        fence(lines, at) ??
        heading(lines, at) ??
        (depth < MAX_DEPTH
          ? (quote(lines, at, depth) ?? list(lines, at, depth))
          : null) ??
        table(lines, at) ??
        paragraph(lines, at, depth);
      out.push(block);
      at = next;
    }
    return out;
  };

  const lines: Line[] = [];
  for (let start = 0; ; ) {
    const end = source.indexOf("\n", start);
    if (end < 0) {
      lines.push({ start, end: source.length });
      break;
    }
    lines.push({ start, end });
    start = end + 1;
  }
  return blocks(lines, 0);
}

function parseInlines(
  source: string,
  pieces: readonly Line[],
  cell: boolean,
): Inline[] {
  let joined = "";
  const offsets: number[] = [];
  pieces.forEach((piece, index) => {
    if (index > 0) {
      joined += "\n";
      offsets.push((pieces[index - 1] as Line).end);
    }
    joined += source.slice(piece.start, piece.end);
    for (let at = piece.start; at < piece.end; at++) offsets.push(at);
  });
  offsets.push(pieces.at(-1)?.end ?? 0);
  const length = joined.length;
  const from = (at: number) => offsets[at] ?? 0;
  const until = (end: number) => from(end - 1) + 1;

  const brackets = new Int32Array(length).fill(-1);
  const ticks = new Int32Array(length).fill(-1);
  const lineEnds = new Int32Array(length + 1).fill(length);
  {
    const open: number[] = [];
    const lastRun = new Map<number, number>();
    const runs: [number, number][] = [];
    for (let at = 0; at < length; at++) {
      const char = joined[at];
      if (char === "\\") at++;
      else if (char === "\n") open.length = 0;
      else if (char === "[") open.push(at);
      else if (char === "]") {
        const opener = open.pop();
        if (opener !== undefined) brackets[opener] = at;
      } else if (char === "`") {
        let run = 1;
        while (joined[at + run] === "`") run++;
        runs.push([at, run]);
        at += run - 1;
      }
    }
    for (let index = runs.length - 1; index >= 0; index--) {
      const [at, run] = runs[index] as [number, number];
      ticks[at] = lastRun.get(run) ?? -1;
      lastRun.set(run, at);
    }
    for (let at = length - 1; at >= 0; at--)
      lineEnds[at] = joined[at] === "\n" ? at : (lineEnds[at + 1] ?? length);
  }

  const textNode = (start: number, end: number): Inline => ({
    kind: "text",
    text: joined.slice(start, end),
    from: from(start),
    to: until(end),
  });
  const destination = (start: number, end: number) => {
    let at = start;
    const limit = Math.min(end, start + MAX_DESTINATION);
    while (joined[at] === " " || joined[at] === "\t") at++;
    let href: string;
    if (joined[at] === "<") {
      const open = ++at;
      while (at < limit && joined[at] !== ">" && joined[at] !== "\n") at++;
      if (joined[at] !== ">") return null;
      href = joined.slice(open, at++);
    } else {
      const open = at;
      let depth = 0;
      for (; at < limit; at++) {
        const char = joined[at] as string;
        if (char <= " ") break;
        if (char === "(") depth++;
        else if (char === ")") {
          if (depth === 0) break;
          depth--;
        }
      }
      href = joined.slice(open, at);
    }
    while (joined[at] === " " || joined[at] === "\t") at++;
    const quote = joined[at];
    if (quote === '"' || quote === "'") {
      at++;
      while (at < limit && joined[at] !== quote && joined[at] !== "\n") at++;
      if (joined[at] !== quote) return null;
      at++;
      while (joined[at] === " " || joined[at] === "\t") at++;
    }
    return joined[at] === ")" && at < end ? { href, end: at + 1 } : null;
  };

  const tokenize = (start: number, end: number, links: boolean): Token[] => {
    const out: Token[] = [];
    let pending = start;
    const flush = (at: number) => {
      if (at > pending) out.push(textNode(pending, at));
    };
    let at = start;
    while (at < end) {
      const char = joined[at];
      if (char === "\n") {
        flush(at);
        out.push(textNode(at, at + 1));
        pending = ++at;
      } else if (
        char === "\\" &&
        at + 1 < end &&
        ESCAPABLE.has(joined[at + 1] as string)
      ) {
        flush(at);
        out.push({
          kind: "text",
          text: joined[at + 1] as string,
          from: from(at),
          to: until(at + 2),
        });
        pending = at += 2;
      } else if (char === "`") {
        let run = 1;
        while (joined[at + run] === "`") run++;
        const close = ticks[at] ?? -1;
        if (
          close > at &&
          close + run <= end &&
          close < (lineEnds[at] ?? length)
        ) {
          flush(at);
          let open = at + run;
          let shut = close;
          let body = joined.slice(open, shut);
          if (
            body.length > 1 &&
            body.startsWith(" ") &&
            body.endsWith(" ") &&
            body.trim()
          ) {
            open++;
            shut--;
            body = body.slice(1, -1);
          }
          out.push({
            kind: "code",
            text: cell ? body.replaceAll("\\|", "|") : body,
            from: from(open),
            to: from(shut),
          });
          pending = at = close + run;
        } else at += run;
      } else if (
        links &&
        (char === "[" || (char === "!" && joined[at + 1] === "["))
      ) {
        const image = char === "!";
        const open = image ? at + 1 : at;
        const close = brackets[open] ?? -1;
        const target =
          close > open && close < end && joined[close + 1] === "("
            ? destination(close + 2, end)
            : null;
        if (!target) {
          at++;
          continue;
        }
        flush(at);
        const range = { from: from(at), to: until(target.end) };
        const safe = SAFE_HREF.test(target.href);
        if (image) {
          const alt = close > open + 1 ? [textNode(open + 1, close)] : [];
          if (safe)
            out.push({
              kind: "link",
              href: target.href,
              children:
                alt.length > 0
                  ? alt
                  : [{ kind: "text", text: target.href, ...range }],
              ...range,
            });
          else out.push(...alt);
        } else {
          const label = resolve(tokenize(open + 1, close, false), 0);
          if (safe)
            out.push({
              kind: "link",
              href: target.href,
              children: label,
              ...range,
            });
          else out.push(...label);
        }
        pending = at = target.end;
      } else if (char === "*" || char === "_") {
        let run = 1;
        while (at + run < end && joined[at + run] === char) run++;
        const before = joined[at - 1] ?? "\n";
        const after = joined[at + run] ?? "\n";
        const beforeSpace = SPACE.test(before);
        const afterSpace = SPACE.test(after);
        const beforePunct = PUNCTUATION.test(before);
        const afterPunct = PUNCTUATION.test(after);
        const left = !afterSpace && (!afterPunct || beforeSpace || beforePunct);
        const right =
          !beforeSpace && (!beforePunct || afterSpace || afterPunct);
        flush(at);
        out.push({
          kind: "delim",
          text: joined.slice(at, at + run),
          open: char === "*" ? left : left && (!right || beforePunct),
          close: char === "*" ? right : right && (!left || afterPunct),
          from: from(at),
          to: until(at + run),
        });
        pending = at += run;
      } else if (
        links &&
        char === "h" &&
        (joined.startsWith("http://", at) || joined.startsWith("https://", at))
      ) {
        BARE_URL.lastIndex = at;
        const found = BARE_URL.exec(joined);
        let length = Math.min(found?.[0].length ?? 0, end - at);
        while (length > 0 && URL_TRAIL.has(joined[at + length - 1] as string))
          length--;
        const href = joined.slice(at, at + length);
        if (href.length === 0) {
          at++;
          continue;
        }
        flush(at);
        out.push({
          kind: "link",
          href,
          children: [textNode(at, at + href.length)],
          from: from(at),
          to: until(at + href.length),
        });
        pending = at += href.length;
      } else at++;
    }
    flush(end);
    return out;
  };
  return resolve(tokenize(0, length, true), 0);
}

function resolve(tokens: readonly Token[], depth: number): Inline[] {
  const closer = new Int32Array(tokens.length).fill(-1);
  const seen = new Map<string, number>();
  for (let at = tokens.length - 1; at >= 0; at--) {
    const token = tokens[at] as Token;
    if (token.kind !== "delim") continue;
    if (token.open) closer[at] = seen.get(token.text) ?? -1;
    if (token.close) seen.set(token.text, at);
  }
  const build = (start: number, end: number, level: number): Inline[] => {
    const out: Inline[] = [];
    for (let at = start; at < end; at++) {
      const token = tokens[at] as Token;
      if (token.kind !== "delim") {
        out.push(token);
        continue;
      }
      const close = closer[at] ?? -1;
      const match = tokens[close];
      if (
        close > at &&
        close < end &&
        match &&
        level < MAX_DEPTH &&
        token.text.length <= 3
      ) {
        const children = build(at + 1, close, level + 1);
        const range = { from: token.from, to: match.to };
        out.push(
          token.text.length === 1
            ? { kind: "em", children, ...range }
            : token.text.length === 2
              ? { kind: "strong", children, ...range }
              : {
                  kind: "em",
                  children: [{ kind: "strong", children, ...range }],
                  ...range,
                },
        );
        at = close;
      } else
        out.push({
          kind: "text",
          text: token.text,
          from: token.from,
          to: token.to,
        });
    }
    return out;
  };
  return build(0, tokens.length, depth);
}

export function cutBlocks(blocks: readonly Block[], limit: number): Block[] {
  const out: Block[] = [];
  for (const block of blocks) {
    if (block.from >= limit) break;
    const cut = block.to <= limit ? block : cutBlock(block, limit);
    if (cut) out.push(cut);
  }
  return out;
}
function cutBlock(block: Block, limit: number): Block | null {
  switch (block.kind) {
    case "paragraph":
    case "heading":
      return { ...block, inlines: cutInlines(block.inlines, limit), to: limit };
    case "code": {
      const lines = block.lines
        .filter((line) => line.from < limit)
        .map((line) =>
          line.to <= limit
            ? line
            : {
                ...line,
                text: line.text.slice(0, limit - line.from),
                to: limit,
              },
        );
      return lines.length > 0
        ? { ...block, lines, closed: false, to: limit }
        : null;
    }
    case "list":
      return {
        ...block,
        items: block.items
          .filter((item) => item.from < limit)
          .map((item) =>
            item.to <= limit
              ? item
              : { ...item, blocks: cutBlocks(item.blocks, limit), to: limit },
          ),
        to: limit,
      };
    case "quote":
      return { ...block, blocks: cutBlocks(block.blocks, limit), to: limit };
    case "table": {
      const cell = (value: TableCell): TableCell =>
        value.to <= limit
          ? value
          : { ...value, inlines: cutInlines(value.inlines, limit), to: limit };
      const row = (value: TableRow): TableRow =>
        value.to <= limit
          ? value
          : { ...value, cells: value.cells.map(cell), to: limit };
      return {
        ...block,
        head: row(block.head),
        rows: block.rows.filter((value) => value.from < limit).map(row),
        to: limit,
      };
    }
  }
}
function cutInlines(nodes: readonly Inline[], limit: number): Inline[] {
  const out: Inline[] = [];
  for (const node of nodes) {
    if (node.from >= limit) break;
    if (node.to <= limit) out.push(node);
    else if (node.kind === "text" || node.kind === "code")
      out.push({
        ...node,
        text: node.text.slice(0, limit - node.from),
        to: limit,
      });
    else
      out.push({
        ...node,
        children: cutInlines(node.children, limit),
        to: limit,
      });
  }
  return out;
}
