"use client";

import CopyCommand from "@/components/CopyCommand";
import { AGENT_PROMPT, INSTALL_MD_URL } from "@/lib/site";
import styles from "./AgentPrompt.module.css";

type AgentPromptProps = {
  // As CopyCommand. "bone" sets an oxide button for a bone field. "ink" sets a bone button
  // for a dark page. Both keep the accent for the page's one action.
  tone?: "accent" | "bone" | "ink";
};

// The one line to paste into a coding agent. The agent reads install.md and does the rest.
export default function AgentPrompt({ tone = "accent" }: AgentPromptProps) {
  return (
    <div className={styles.prompt}>
      <CopyCommand command={AGENT_PROMPT} variant="prose" tone={tone} label="Copy" />
      <p className={styles.note}>
        Works in Claude Code, Codex, GitHub Copilot CLI and VS Code. Read the steps first:{" "}
        <a href={INSTALL_MD_URL}>install.md</a>
      </p>
    </div>
  );
}
