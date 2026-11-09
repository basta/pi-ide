// Dev-only pi extension: `/ask-test` asks one of each blocking dialog, so pide's
// dialog cards can be tried by hand. Load with: PIDE_PI_ARGS="-e dev/pi-ask-test.ts"
export default function (pi: any) {
  pi.registerCommand("ask-test", {
    description: "Ask one of each dialog (pide dev test)",
    handler: async (_args: string, ctx: any) => {
      const ok = await ctx.ui.confirm("Allow this command?", "rm -rf target/debug/incremental");
      const pick = await ctx.ui.select("Which model for the review?", ["Kimi K2.6", "GLM 5.2", "Sonnet 5.5"]);
      const name = await ctx.ui.input("Name for the new branch", "e.g. crlf-save-fix");
      const msg = await ctx.ui.editor("Edit the commit message", "Keep CRLF endings on save\n\nBuffers remember their line ending.");
      ctx.ui.notify(`answers: confirm=${ok} select=${pick} input=${name} editor=${JSON.stringify(msg)}`, "info");
    },
  });
}
