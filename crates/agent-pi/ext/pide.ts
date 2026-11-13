// pide's own pi extension (collaborator mode, docs/COLLAB.md). pide writes this file
// to its state folder at startup and loads it with `-e` into every pi it spawns.
//
// Transport: nothing extra. pi already streams every tool call to pide over the RPC
// stdout as `tool_execution_end`, and that record carries the tool result's
// `details` (docs/json.md). Each tool here validates its input, reads the anchor
// line text from disk, and returns `details: { pide: 1, kind, ... }`; pide picks
// those records out in agent-pi/src/wire.rs. No socket, no extra protocol.
//
// Plain JSON Schema for parameters (pi validates any JSON Schema), so the file has
// no imports and loads from any folder.

import { readFileSync } from "node:fs";
import { isAbsolute, relative, resolve } from "node:path";

/** Pins allowed per user prompt (a run); more are refused so pi stays sparing. */
const MAX_PINS = 5;

export default function (pi: any) {
  let pins = 0;
  pi.on("agent_start", async () => {
    pins = 0;
  });

  const abs = (cwd: string, p: string) => {
    const clean = String(p ?? "").replace(/^@/, "");
    return isAbsolute(clean) ? clean : resolve(cwd, clean);
  };
  /** `path` relative to the project when it is inside it (pide joins it to its own root,
   *  which may be spelled differently from `ctx.cwd`, e.g. /tmp vs /private/tmp). */
  const rel = (cwd: string, path: string) => {
    const r = relative(cwd, path);
    return r && !r.startsWith("..") && !isAbsolute(r) ? r : path;
  };
  /** Lines of a file (`null` if it can't be read). */
  const lines = (path: string): string[] | null => {
    try {
      return readFileSync(path, "utf8").split("\n");
    } catch {
      return null;
    }
  };
  const text = (msg: string) => [{ type: "text", text: msg }];

  pi.registerTool({
    name: "ide_pin",
    label: "Pin in editor",
    description:
      "Point the user at a place in the code, shown as a mark and a note in their editor. " +
      "Use sparingly: only for something the user must look at or decide (a risk, a choice you made, " +
      `a place that needs their input). At most ${MAX_PINS} per prompt. Lines are 1-based.`,
    promptSnippet: "Point the user at code in their editor with a short note",
    promptGuidelines: [
      "Use ide_pin only for code the user must look at or decide on; never to narrate routine work.",
    ],
    parameters: {
      type: "object",
      properties: {
        path: { type: "string", description: "File path, relative to the project or absolute" },
        start_line: { type: "integer", minimum: 1, description: "First line (1-based)" },
        end_line: { type: "integer", minimum: 1, description: "Last line (1-based), optional" },
        note: { type: "string", description: "One or two sentences for the user" },
      },
      required: ["path", "start_line", "note"],
    },
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      if (pins >= MAX_PINS) {
        return {
          content: text(`Not pinned: already ${MAX_PINS} pins for this prompt. Mention it in your reply instead.`),
          details: { pide: 1, kind: "refused" },
          isError: true,
        };
      }
      const path = abs(ctx.cwd, params.path);
      const file = lines(path);
      if (!file) {
        return { content: text(`Not pinned: can't read ${params.path}.`), details: { pide: 1, kind: "refused" }, isError: true };
      }
      const start = Math.min(Math.max(1, params.start_line), file.length);
      const end = Math.max(start, Math.min(params.end_line ?? start, file.length));
      pins += 1;
      return {
        content: text(`Pinned ${params.path}:${start} for the user.`),
        details: {
          pide: 1,
          kind: "pin",
          path: rel(ctx.cwd, path),
          line: start,
          end_line: end,
          text: file[start - 1] ?? "",
          note: String(params.note ?? ""),
        },
      };
    },
  });

  pi.registerTool({
    name: "ide_explain_change",
    label: "Explain change",
    description:
      "Tell the user why you changed a file: a one-line summary, and optionally a short note for " +
      "individual changed places (line numbers in the file as it is now, 1-based). Call it once per " +
      "changed file when you are done changing code, not after every edit. Keep notes short; " +
      "only add hunk notes where the reason isn't obvious.",
    promptSnippet: "Explain to the user why a file you changed was changed",
    promptGuidelines: [
      "After you finish changing code, call ide_explain_change once for each file you changed, with a one-line summary.",
    ],
    parameters: {
      type: "object",
      properties: {
        path: { type: "string", description: "File path, relative to the project or absolute" },
        summary: { type: "string", description: "One line: why this file changed" },
        hunks: {
          type: "array",
          description: "Optional notes for changed places",
          items: {
            type: "object",
            properties: {
              line: { type: "integer", minimum: 1, description: "Line in the current file (1-based)" },
              note: { type: "string" },
            },
            required: ["line", "note"],
          },
        },
      },
      required: ["path", "summary"],
    },
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      const path = abs(ctx.cwd, params.path);
      const file = lines(path) ?? [];
      const hunks = (Array.isArray(params.hunks) ? params.hunks : []).slice(0, 12).map((h: any) => {
        const line = Math.max(1, Number(h.line) || 1);
        return { line, text: file[line - 1] ?? "", note: String(h.note ?? "") };
      });
      return {
        content: text(`Noted for ${params.path}.`),
        details: { pide: 1, kind: "explain", path: rel(ctx.cwd, path), summary: String(params.summary ?? ""), hunks },
      };
    },
  });

  pi.registerTool({
    name: "ide_progress",
    label: "Progress",
    description:
      "Show the user a short checklist of the steps of a longer task, with which are done. " +
      "Send the whole list each time; call it when the plan is set and when a step finishes. " +
      "Skip it for small tasks.",
    promptSnippet: "Show the user a checklist of steps for a longer task",
    parameters: {
      type: "object",
      properties: {
        steps: {
          type: "array",
          items: {
            type: "object",
            properties: { text: { type: "string" }, done: { type: "boolean" } },
            required: ["text", "done"],
          },
        },
      },
      required: ["steps"],
    },
    async execute(_id: string, params: any) {
      const steps = (Array.isArray(params.steps) ? params.steps : [])
        .slice(0, 20)
        .map((s: any) => ({ text: String(s.text ?? ""), done: Boolean(s.done) }));
      const done = steps.filter((s: any) => s.done).length;
      return {
        content: text(`Checklist shown (${done}/${steps.length} done).`),
        details: { pide: 1, kind: "progress", steps },
      };
    },
  });
}
