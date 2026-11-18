import { fileURLToPath } from "node:url";
import { build, defineConfig, type Plugin } from "vite";

// Mermaid is built on its own, apart from the page. In one build, Mermaid's
// chunks import the page's main chunk by its hashed name, so any change to
// the page would rename all of Mermaid's files in src/dashboard/channels.
// Built apart, they change only when Mermaid or src/diagramDraw.ts does.
function diagramBundle(): Plugin {
  let files: { fileName: string; source: string | Uint8Array }[] = [];
  return {
    name: "namedrop-diagram-bundle",
    apply: "build",
    async config(page) {
      const built = await build({
        configFile: false,
        logLevel: "warn",
        resolve: {
          alias: {
            "elkjs/lib/elk.bundled.js": fileURLToPath(
              new URL("src/diagramNoElk.ts", import.meta.url),
            ),
          },
        },
        build: {
          write: false,
          target: page.build?.target,
          rolldownOptions: {
            input: "src/diagramDraw.ts",
            // The page imports the entry's `draw`, so it must keep that name.
            preserveEntrySignatures: "strict",
            output: {
              entryFileNames: "diagram/draw-[hash].js",
              chunkFileNames: "diagram/[name]-[hash].js",
            },
          },
        },
      });
      const result = Array.isArray(built) ? built[0] : built;
      if (!result || !("output" in result))
        throw new Error("the diagram build produced no files");
      const entry = result.output.find(
        (file) => file.type === "chunk" && file.isEntry,
      );
      if (!entry) throw new Error("the diagram build has no entry");
      files = result.output.map((file) => ({
        fileName: file.fileName,
        source: file.type === "chunk" ? file.code : file.source,
      }));
      return {
        define: { __DIAGRAM_ENTRY__: JSON.stringify(`/${entry.fileName}`) },
      };
    },
    generateBundle() {
      for (const file of files) this.emitFile({ type: "asset", ...file });
    },
  };
}

export default defineConfig({
  build: { target: "es2023" },
  plugins: [diagramBundle()],
});
