// Our Pi extension: every tool call goes past Tool Gateway first, and the agent gets
// the control plane's tools (D1, D76).
//
// Fail-close (D38): if the check cannot be made, the call is blocked.

import type { ExtensionAPI, ToolCallEvent } from "@earendil-works/pi-coding-agent";
import { call } from "./api.ts";
import { FILE_TOOLS, resolveReal } from "./paths.ts";
import { tools } from "./tools.ts";

type Verdict = { verdict: "allow" } | { verdict: "deny"; reason: string };

export default function (pi: ExtensionAPI) {
  for (const tool of tools) {
    pi.registerTool(tool);
  }

  pi.on("tool_call", async (event, ctx) => {
    try {
      const verdict = (await check(event, ctx.cwd)) as Verdict;
      return verdict.verdict === "allow" ? undefined : { block: true, reason: verdict.reason };
    } catch (error) {
      return { block: true, reason: `the call could not be checked: ${(error as Error).message}` };
    }
  });
}

async function check(event: ToolCallEvent, cwd: string): Promise<unknown> {
  let path: string | undefined;
  const optional = FILE_TOOLS[event.toolName];
  if (optional !== undefined) {
    const input = event.input as { path?: string };
    if (input.path !== undefined || optional) {
      path = resolveReal(cwd, input.path ?? ".");
      // The tool works on exactly the path Gateway checked, not on its own reading of it.
      input.path = path;
    }
  }
  return call("POST", "/v1/gateway/check", { tool: event.toolName, path: path ?? null });
}
