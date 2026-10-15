// ABOUTME: Starts automatic session naming alongside the first main-agent turn.
// ABOUTME: Uses an independent in-memory agent so naming never enters the live session queue.

import { type ExtensionAPI, ModelRuntime } from "@earendil-works/pi-coding-agent";
import { generateTitleForPrompt } from "./session-title";

type AutomaticTitleOptions = {
  generateTitle?: (prompt: string, model: unknown) => Promise<string>;
};

async function generateWithIndependentAgent(prompt: string, model: unknown): Promise<string> {
  const modelRuntime = await ModelRuntime.create();
  return generateTitleForPrompt(prompt, { model, modelRuntime });
}

export function registerAutomaticSessionTitle(
  pi: ExtensionAPI,
  options: AutomaticTitleOptions = {},
): void {
  const generateTitle = options.generateTitle ?? generateWithIndependentAgent;
  let attempted = false;
  let inFlight = false;

  pi.on("before_agent_start", (event, ctx) => {
    // pi 1.2.0 split this event: "custom" runs started via pi.sendMessage carry
    // `message` instead of `prompt`; naming stays anchored to real user prompts.
    if (typeof event.prompt !== "string") return;
    const prompt = event.prompt.trim();
    if (!prompt || attempted || inFlight || pi.getSessionName()) return;

    attempted = true;
    inFlight = true;
    void generateTitle(prompt, ctx.model)
      .then((title) => {
        // Preserve a manual or externally generated name chosen while the
        // independent title agent was still running.
        if (!pi.getSessionName()) pi.setSessionName(title);
      })
      .catch(() => {})
      .finally(() => {
        inFlight = false;
      });
  });
}
