import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type AttachedImage,
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
    "@lead look\nsecond line",
    "  indented",
  ]) {
    for (const images of [[a], [a, b]]) {
      const split = splitImages(withImages(typed, images));
      assert.equal(split.text, typed);
      assert.deepEqual(split.images, images);
    }
    assert.equal(withImages(typed, []), typed);
    assert.deepEqual(splitImages(typed), { text: typed, images: [] });
  }
});

test("only lines at the end that name a stored image are images", () => {
  for (const source of [
    `${a.line}\nthen more text`,
    "hi\nImage: channel-images/x.png",
    `hi\nImage: /state/other/${a.name}`,
    `hi\nImage: /state/channel-images/${"A".repeat(64)}.png`,
    `hi\nImage: /state/channel-images/${"a".repeat(64)}.svg`,
    `hi\nImage: /state/channel-images/sub/${a.name}`,
    `hi\n image: /state/channel-images/${a.name}`,
  ])
    assert.deepEqual(splitImages(source), { text: source, images: [] });
  assert.deepEqual(splitImages(`hi\n\n${a.line}`), { text: "hi", images: [a] });
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
