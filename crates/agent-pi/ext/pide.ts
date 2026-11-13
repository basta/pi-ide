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
  // When pi last changed a file: the editor's language servers need a moment to
  // re-check it, so ide_diagnostics waits that long first.
  let lastEdit = 0;
  pi.on("tool_result", async (event: any) => {
    if (["edit", "write", "bash"].includes(event.toolName)) lastEdit = Date.now();
  });
  const SETTLE_MS = 2000;

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
  /** Lines in a file (a trailing newline's empty element isn't one). */
  const lineCount = (file: string[]) => (file.length > 1 && file[file.length - 1] === "" ? file.length - 1 : file.length);
  /** The model's line numbers are often off by a few; the quoted code isn't. The
   *  1-based line nearest to `line` whose text contains `code` (whitespace at the
   *  ends ignored), or null if no line does. No `code`: `line`, clamped. */
  const snap = (file: string[], line: number, code: unknown): number | null => {
    const count = lineCount(file);
    const want = Math.min(Math.max(1, Number(line) || 1), count);
    // Several lines quoted: the first non-blank one. Too short to be distinctive
    // ("}", "")): trust the line number instead.
    const first = typeof code === "string" ? code.split("\n").map((l) => l.trim()).find((l) => l) ?? "" : "";
    const needle = first.length >= 3 ? first : "";
    if (!needle) return want;
    let best: number | null = null;
    for (let i = 0; i < count; i++) {
      if (file[i].includes(needle) && (best === null || Math.abs(i + 1 - want) < Math.abs(best - want))) best = i + 1;
    }
    return best;
  };
  /** The 1-based range starting at `start` that is as long as the model's
   *  [line, end_line] (one line when end_line is missing or before line). */
  const rangeEnd = (file: string[], start: number, line: unknown, endLine: unknown): number => {
    const a = Number(line) || 0;
    const b = Number(endLine) || 0;
    const len = a > 0 && b > a ? b - a : 0;
    return Math.min(start + len, lineCount(file));
  };
  const CODE_DOC = "The text of that line (or a distinctive part of it), copied exactly from the file; " +
    "it places the note even if the line number is off.";

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
        code: { type: "string", description: CODE_DOC },
        end_line: { type: "integer", minimum: 1, description: "Last line (1-based), optional" },
        note: { type: "string", description: "One or two sentences for the user" },
      },
      required: ["path", "start_line", "code", "note"],
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
      const count = lineCount(file);
      const start = snap(file, params.start_line, params.code);
      if (start === null) {
        return {
          content: text(`Not pinned: no line in ${params.path} contains ${JSON.stringify(params.code)}. Read the file and try again with code copied exactly.`),
          details: { pide: 1, kind: "refused" },
          isError: true,
        };
      }
      const end = rangeEnd(file, start, params.start_line, params.end_line);
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
          // The pinned lines (at most 8), shown with the note in pide's chat.
          snippet: file.slice(start - 1, Math.min(end, start + 7)),
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
              code: { type: "string", description: CODE_DOC },
              note: { type: "string" },
            },
            required: ["line", "code", "note"],
          },
        },
      },
      required: ["path", "summary"],
    },
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      const path = abs(ctx.cwd, params.path);
      const file = lines(path) ?? [];
      const missed: string[] = [];
      const hunks = (Array.isArray(params.hunks) ? params.hunks : []).slice(0, 12).flatMap((h: any) => {
        const line = file.length ? snap(file, h.line, h.code) : Math.max(1, Number(h.line) || 1);
        if (line === null) {
          missed.push(JSON.stringify(h.code));
          return [];
        }
        return [{ line, text: file[line - 1] ?? "", note: String(h.note ?? "") }];
      });
      const miss = missed.length
        ? ` Left out notes whose code isn't in the file: ${missed.join(", ")}. Call again with code copied exactly to add them.`
        : "";
      return {
        content: text(`Noted for ${params.path}.${miss}`),
        details: { pide: 1, kind: "explain", path: rel(ctx.cwd, path), summary: String(params.summary ?? ""), hunks },
      };
    },
  });

  /** Ask pide for something it knows (its language servers' view, ...). pide answers a
   *  `ctx.ui.input` titled "pide:query" at once, without showing it, with JSON in `value`
   *  (docs/rpc-extension-ui.md: dialog requests wait for an extension_ui_response).
   *  Outside pide nobody answers, so it times out and returns null. */
  const query = async (ctx: any, payload: any): Promise<any> => {
    if (!ctx.hasUI) return null;
    const value = await ctx.ui.input("pide:query", JSON.stringify(payload), { timeout: 5000 });
    if (typeof value !== "string") return null;
    try {
      return JSON.parse(value);
    } catch {
      return null;
    }
  };

  pi.registerTool({
    name: "ide_diagnostics",
    label: "Look at problems",
    description:
      "See the errors and warnings the user's editor shows (from its language servers), for given " +
      "files or for every file it knows about. Use it after changing code to check you didn't leave " +
      "problems the user would see, instead of guessing. Pass the files you changed: files the editor " +
      "hasn't opened are checked by its language server first (a few seconds).",
    promptSnippet: "See the errors and warnings the user's editor shows for files",
    promptGuidelines: [
      "After changing code, call ide_diagnostics on the files you changed and fix new errors before you finish.",
    ],
    parameters: {
      type: "object",
      properties: {
        paths: {
          type: "array",
          items: { type: "string" },
          description: "Files to check (relative to the project or absolute); omit for all known files",
        },
      },
    },
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      const paths = (Array.isArray(params.paths) ? params.paths : []).map((p: any) => rel(ctx.cwd, abs(ctx.cwd, p)));
      const wait = lastEdit + SETTLE_MS - Date.now();
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
      // Files the editor hasn't opened go to their language server first; pide
      // says they're pending until it has had a moment to check them.
      // attempt 0 makes pide re-read them; later attempts only trust those reads.
      let answer = await query(ctx, { kind: "diagnostics", paths, attempt: 0 });
      for (let i = 1; i <= 8 && answer && Array.isArray(answer.pending) && answer.pending.length; i++) {
        await new Promise((r) => setTimeout(r, i === 1 ? 700 : 2000));
        answer = await query(ctx, { kind: "diagnostics", paths, attempt: i });
      }
      if (!answer) {
        return {
          content: text("No answer from the editor (pide isn't running this session)."),
          details: { pide: 1, kind: "refused" },
          isError: true,
        };
      }
      const items: any[] = Array.isArray(answer.diagnostics) ? answer.diagnostics : [];
      const lines = items.map(
        (d) => `${d.path}:${d.line}:${d.column} ${d.severity}: ${d.message}${d.source ? ` [${d.source}]` : ""}`,
      );
      const notes: string[] = Array.isArray(answer.notes) ? answer.notes : [];
      if (Array.isArray(answer.pending) && answer.pending.length) {
        notes.push(`Still being checked (ask again shortly): ${answer.pending.join(", ")}.`);
      }
      const body = [
        items.length ? `${items.length} problem(s):` : "No problems shown for these files.",
        ...lines,
        ...notes,
      ].join("\n");
      return {
        content: text(body),
        details: { pide: 1, kind: "diagnostics", count: items.length },
      };
    },
  });

  pi.registerTool({
    name: "ide_tour",
    label: "Guided tour",
    description:
      "Give the user a guided tour of how something works: an ordered list of places in the code, " +
      "across files, each with a short note on what happens there and how it leads to the next. " +
      "pide shows it as a walkthrough the user steps through in the editor. Call it once per tour, " +
      "only when the user asks how something works or asks for a tour; 4 to 15 stops. Lines are 1-based.",
    promptSnippet: "Give the user a guided tour through the code, stop by stop",
    parameters: {
      type: "object",
      properties: {
        title: { type: "string", description: "What the tour shows, a few words" },
        stops: {
          type: "array",
          items: {
            type: "object",
            properties: {
              path: { type: "string" },
              line: { type: "integer", minimum: 1 },
              code: { type: "string", description: CODE_DOC },
              end_line: { type: "integer", minimum: 1 },
              note: { type: "string" },
            },
            required: ["path", "line", "code", "note"],
          },
        },
      },
      required: ["title", "stops"],
    },
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      const stops: any[] = [];
      const skipped: string[] = [];
      for (const st of (Array.isArray(params.stops) ? params.stops : []).slice(0, 15)) {
        const path = abs(ctx.cwd, st.path);
        const file = lines(path);
        if (!file) {
          skipped.push(String(st.path));
          continue;
        }
        const count = lineCount(file);
        const start = snap(file, st.line, st.code);
        if (start === null) {
          skipped.push(`${st.path} (no line contains ${JSON.stringify(st.code)})`);
          continue;
        }
        const end = rangeEnd(file, start, st.line, st.end_line);
        stops.push({
          path: rel(ctx.cwd, path),
          line: start,
          end_line: end,
          text: file[start - 1] ?? "",
          snippet: file.slice(start - 1, Math.min(end, start + 7)),
          note: String(st.note ?? ""),
        });
      }
      if (!stops.length) {
        return {
          content: text(`No tour: no stop could be placed. ${skipped.join("; ")}`),
          details: { pide: 1, kind: "refused" },
          isError: true,
        };
      }
      const note = skipped.length ? ` Left out: ${skipped.join("; ")}.` : "";
      return {
        content: text(`Tour ready with ${stops.length} stops: the user starts it from the tour card in pide's chat (it isn't open yet).${note}`),
        details: { pide: 1, kind: "tour", title: String(params.title ?? "Tour"), stops },
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
