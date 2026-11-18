import { createContext } from "react";
import type { ImageUpload } from "../images.ts";

// Stores one image for a message. Without it a composer takes text only.
export const ImageUploadContext = createContext<
  ((file: Blob) => Promise<ImageUpload>) | null
>(null);
