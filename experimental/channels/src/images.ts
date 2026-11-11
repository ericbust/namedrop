// Images attached to a message. A message names each one on a line of its
// own at its end, so a draft carries its images in its text.

export type AttachedImage = {
  // The stored file's name, which the server serves under /api/images/.
  name: string;
  // The line the message carries, `Image: ` and the file's absolute path.
  line: string;
};
export type ImageRefusal = "type" | "size" | "count" | "failed";
export type ImageUpload =
  | { kind: "stored"; image: AttachedImage }
  | { kind: "refused"; why: ImageRefusal };

export const MAX_IMAGES = 4;
export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
export const IMAGE_TYPES: readonly string[] = [
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
];
const NAME = /^[0-9a-f]{64}\.(?:png|jpg|gif|webp)$/;
const LINE = /^Image: \/(?:[^\n]*\/)?channel-images\/([^/\n]+)$/;

export function isImageName(name: unknown): name is string {
  return typeof name === "string" && NAME.test(name);
}
export function imageUrl(name: string): string {
  return `/api/images/${name}`;
}
function imageOf(line: string): AttachedImage | null {
  const name = LINE.exec(line)?.[1];
  return name !== undefined && NAME.test(name) ? { name, line } : null;
}

// The text before the image lines, exactly as typed, and the images.
export function splitImages(source: string): {
  text: string;
  images: readonly AttachedImage[];
} {
  const images: AttachedImage[] = [];
  let text = source;
  for (;;) {
    const start = text.lastIndexOf("\n") + 1;
    const image = imageOf(text.slice(start));
    if (image === null) break;
    images.unshift(image);
    text = text.slice(0, Math.max(0, start - 1));
    if (start === 0) break;
  }
  if (images.length === 0) return { text: source, images };
  return { text: text.endsWith("\n") ? text.slice(0, -1) : text, images };
}
export function withImages(
  text: string,
  images: readonly AttachedImage[],
): string {
  return images.length === 0
    ? text
    : `${text}\n\n${images.map((image) => image.line).join("\n")}`;
}

// The image files of a paste or a drop. A paste that also carries text is
// text: a spreadsheet or a document copies its cells as a picture too.
export function imageFiles(
  data: Pick<DataTransfer, "files" | "getData"> | null,
  kind: "paste" | "drop",
): File[] {
  if (data === null) return [];
  if (kind === "paste" && data.getData("text/plain") !== "") return [];
  return [...data.files].filter((file) => file.type.startsWith("image/"));
}

// Why a file can't be attached, before any upload.
export function refusalOf(
  file: Pick<File, "type" | "size">,
  attached: number,
): ImageRefusal | null {
  if (attached >= MAX_IMAGES) return "count";
  if (!IMAGE_TYPES.includes(file.type)) return "type";
  if (file.size > MAX_IMAGE_BYTES) return "size";
  return null;
}
