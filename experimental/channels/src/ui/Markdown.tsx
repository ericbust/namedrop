import { Fragment, type ReactNode, useMemo } from "react";
import {
  type Align,
  type Block,
  cutBlocks,
  type Inline,
  parseMarkdown,
  type TableCell,
} from "../markdown.ts";

// The server reads "\@name" as a reference that wakes no one, so the reader sees just the name.
const ESCAPED_MENTION = /\\(?=@[a-z0-9])/g;
export const prose = (text: string) => text.replace(ESCAPED_MENTION, "");

function inlineNode(node: Inline, links: boolean): ReactNode {
  switch (node.kind) {
    case "text":
      return prose(node.text);
    case "strong":
      return <strong key={node.from}>{inlines(node.children, links)}</strong>;
    case "em":
      return <em key={node.from}>{inlines(node.children, links)}</em>;
    case "code":
      return <code key={node.from}>{node.text}</code>;
    case "link":
      return links ? (
        <a key={node.from} href={node.href} target="_blank" rel="noreferrer">
          {inlines(node.children, links)}
        </a>
      ) : (
        <Fragment key={node.from}>{inlines(node.children, links)}</Fragment>
      );
  }
}
function inlines(nodes: readonly Inline[], links: boolean): ReactNode[] {
  return nodes.map((node) => inlineNode(node, links));
}
const codeText = (block: Extract<Block, { kind: "code" }>) =>
  block.lines.map((line) => `${line.text}\n`).join("");

// The body is pre-wrap, so a paragraph renders as its own source text in the
// flow and keeps the author's line breaks; only list, quote, code, heading and
// table blocks become elements.
function blockNode(block: Block, tail: ReactNode): ReactNode {
  switch (block.kind) {
    case "paragraph":
      return (
        <Fragment key={block.from}>
          {inlines(block.inlines, true)}
          {tail}
        </Fragment>
      );
    case "heading":
      return (
        <div key={block.from} className="md-heading" data-level={block.level}>
          {inlines(block.inlines, true)}
          {tail}
        </div>
      );
    case "code":
      return (
        <Fragment key={block.from}>
          <pre>
            <code>{codeText(block)}</code>
          </pre>
          {tail}
        </Fragment>
      );
    case "list": {
      const List = block.ordered ? "ol" : "ul";
      const last = block.items.at(-1);
      return (
        <List
          key={block.from}
          start={block.ordered && block.start !== 1 ? block.start : undefined}
        >
          {block.items.map((item) => (
            <li key={item.from}>
              {nested(item.blocks, item === last ? tail : null)}
            </li>
          ))}
        </List>
      );
    }
    case "quote":
      return (
        <blockquote key={block.from}>{nested(block.blocks, tail)}</blockquote>
      );
    case "table":
      return (
        <Fragment key={block.from}>
          <div className="md-table">
            <table>
              <thead>
                <tr>
                  <Cells cells={block.head.cells} align={block.align} head />
                </tr>
              </thead>
              <tbody>
                {block.rows.map((row) => (
                  <tr key={row.from}>
                    <Cells cells={row.cells} align={block.align} />
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {tail}
        </Fragment>
      );
  }
}
function Cells({
  cells,
  align,
  head = false,
}: {
  cells: readonly TableCell[];
  align: readonly Align[];
  head?: boolean;
}) {
  const Cell = head ? "th" : "td";
  return cells.map((cell, column) => (
    <Cell key={cell.from} data-align={align[column] ?? undefined}>
      {inlines(cell.inlines, true)}
    </Cell>
  ));
}
function nested(blocks: readonly Block[], tail: ReactNode): ReactNode[] {
  return blocks.map((block, index) => {
    const previous = blocks[index - 1];
    const node = blockNode(block, index === blocks.length - 1 ? tail : null);
    return previous?.kind === "paragraph" && block.kind === "paragraph" ? (
      <Fragment key={block.from}>
        {"\n\n"}
        {node}
      </Fragment>
    ) : (
      node
    );
  });
}

export function Markdown({
  source,
  limit = source.length,
  tail = null,
}: {
  source: string;
  limit?: number;
  tail?: ReactNode;
}) {
  const parsed = useMemo(() => parseMarkdown(source), [source]);
  const blocks = cutBlocks(parsed, limit);
  const last = blocks.at(-1);
  const inside =
    last?.kind === "list" || last?.kind === "quote" || last?.kind === "heading";
  const out: ReactNode[] = [];
  let at = 0;
  let flowing = true;
  for (const block of blocks) {
    const paragraph = block.kind === "paragraph";
    if (paragraph && flowing)
      out.push(source.slice(at, Math.min(block.from, limit)));
    out.push(blockNode(block, inside && block === last ? tail : null));
    at = block.to;
    flowing = paragraph;
  }
  if (flowing) out.push(source.slice(at, limit));
  if (!inside) out.push(tail);
  return out;
}

function flatInline(block: Block, links: boolean): ReactNode[] {
  switch (block.kind) {
    case "paragraph":
    case "heading":
      return inlines(block.inlines, links);
    case "code":
      return [<code key={block.from}>{codeText(block).slice(0, -1)}</code>];
    case "list":
      return block.items.flatMap((item, index) => [
        index > 0 ? "\n" : null,
        ...flat(item.blocks, links),
      ]);
    case "quote":
      return flat(block.blocks, links);
    case "table":
      return [block.head, ...block.rows].flatMap((row, index) => [
        index > 0 ? "\n" : null,
        ...row.cells.flatMap((cell, column) => [
          column > 0 ? " | " : null,
          ...inlines(cell.inlines, links),
        ]),
      ]);
  }
}
function flat(blocks: readonly Block[], links: boolean): ReactNode[] {
  return blocks.flatMap((block, index) => [
    index > 0 ? "\n" : null,
    ...flatInline(block, links),
  ]);
}

// Hook rows sit inside a paragraph and reply previews inside a button, where
// block elements cannot go and a link cannot nest, so they keep inline styles only.
export function MarkdownLine({
  source,
  limit = source.length,
  links = false,
}: {
  source: string;
  limit?: number;
  links?: boolean;
}) {
  const parsed = useMemo(() => parseMarkdown(source), [source]);
  const blocks = cutBlocks(parsed, limit);
  const out: ReactNode[] = [];
  let at = 0;
  let previous: Block | null = null;
  for (const block of blocks) {
    if (
      previous === null ||
      (previous.kind === "paragraph" && block.kind === "paragraph")
    )
      out.push(source.slice(at, Math.min(block.from, limit)));
    else out.push("\n");
    out.push(<Fragment key={block.from}>{flatInline(block, links)}</Fragment>);
    at = block.to;
    previous = block;
  }
  if (previous === null || previous.kind === "paragraph")
    out.push(source.slice(at, limit));
  return out;
}
