import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  type AttachedImage,
  draftImages,
  imageFiles,
  isImageName,
  MAX_IMAGE_BYTES,
  refusalOf,
  splitImages,
  withImages,
} from "./images.ts";

const image = (digit: string, type = "png"): AttachedImage => {
  const name = `${digit.repeat(64)}.${type}`;
  return { name, line: `Image: /home/me/state/channel-images/${name}` };
};
const a = image("a");
const b = image("b", "webp");

test("a draft keeps its typed text exactly beside its images", () => {
  for (const typed of [
    "",
    "hello",
    "hello ",
    "hello\n",
    "hello\n\n",
    "hello \t",
    "@lead look\nsecond line",
    "  indented",
  ]) {
    for (const images of [[a], [a, b]]) {
      const split = draftImages(withImages(typed, images));
      assert.equal(split.text, typed);
      assert.deepEqual(split.images, images);
    }
    assert.equal(withImages(typed, []), typed);
    assert.deepEqual(draftImages(typed), { text: typed, images: [] });
  }
});

// The server's `split` reads the same file, so the page hides the lines the
// server hides.
const shared: {
  directory: string;
  cases: { name: string; body: string; text: string; images: string[] }[];
} = JSON.parse(
  readFileSync(
    new URL(
      "../../../tests/fixtures/channel_images/lines.json",
      import.meta.url,
    ),
    "utf8",
  ),
);

test("image lines are split off as the cases shared with the server say", () => {
  assert.ok(shared.cases.length > 20);
  for (const { name, body, text, images } of shared.cases) {
    const split = splitImages(body);
    assert.equal(split.text, text, name);
    assert.deepEqual(
      split.images.map((image) => image.name),
      images,
      name,
    );
    for (const image of split.images)
      assert.equal(
        image.line,
        `Image: ${shared.directory}/${image.name}`,
        name,
      );
    assert.deepEqual(
      draftImages(body).images.map((image) => image.name),
      images,
      `${name}, as a draft`,
    );
  }
});

test("a name is a stored image's only in its exact form", () => {
  assert.equal(isImageName(a.name), true);
  assert.equal(isImageName(`../${a.name}`), false);
  assert.equal(isImageName(7), false);
});

function clipboard(files: File[], text = "") {
  return {
    files: files as unknown as FileList,
    getData: (type: string) => (type === "text/plain" ? text : ""),
  };
}
const png = new File(["x"], "shot.png", { type: "image/png" });

test("a paste with text is text and a drop keeps its images", () => {
  const note = new File(["x"], "note.txt", { type: "text/plain" });
  assert.deepEqual(imageFiles(clipboard([png, note]), "paste"), [png]);
  assert.deepEqual(imageFiles(clipboard([png], "cell A1"), "paste"), []);
  assert.deepEqual(imageFiles(clipboard([png], "dragged"), "drop"), [png]);
  assert.deepEqual(imageFiles(clipboard([note]), "paste"), []);
  assert.deepEqual(imageFiles(null, "paste"), []);
});

test("a file is refused before upload for its type, size or the count", () => {
  assert.equal(refusalOf(png, 0), null);
  assert.equal(refusalOf(png, 3), null);
  assert.equal(refusalOf(png, 4), "count");
  assert.equal(refusalOf({ type: "image/svg+xml", size: 10 }, 0), "type");
  assert.equal(refusalOf({ type: "image/avif", size: 10 }, 0), "type");
  assert.equal(
    refusalOf({ type: "image/jpeg", size: MAX_IMAGE_BYTES }, 0),
    null,
  );
  assert.equal(
    refusalOf({ type: "image/jpeg", size: MAX_IMAGE_BYTES + 1 }, 0),
    "size",
  );
});
