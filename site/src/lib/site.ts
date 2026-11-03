export const SITE_URL = "https://atticdb.com";
export const GITHUB_URL = "https://github.com/Clawdlinux/atticdb";
export const DOCS_ORIGIN = "https://docs.atticdb.com";
export const DOCS_URL = DOCS_ORIGIN;
export const INSTALL_CMD =
  "curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh";
// The one line you paste into a coding agent. The agent follows install.md from there.
export const INSTALL_MD_URL = "https://atticdb.com/install.md";
export const AGENT_PROMPT = `Set up AtticDB memory for me. Follow ${INSTALL_MD_URL} step by step.`;
export const WAITLIST_FORM_URL =
  "https://docs.google.com/forms/d/e/1FAIpQLSfliAcFY85SBQAiUpZnllibyhD3xhFpjWmTk1ynmpUWGRIVcQ/viewform";

// The form's "Which plan are you interested in?" field. Values must match its choices exactly,
// or Google Forms ignores the prefill.
const WAITLIST_PLAN_FIELD = "entry.690882465";
export const WAITLIST_PLAN_CHOICES = {
  pro: "Cloud Pro: 1 person, all your machines ($15 a month, planned)",
  team: "Cloud Team: shared team memory ($29 per seat a month, planned)",
  vpc: "Your cloud: your VPC or air-gapped",
} as const;
export type WaitlistPlan = keyof typeof WAITLIST_PLAN_CHOICES;

export function waitlistUrl(plan?: WaitlistPlan): string {
  if (!plan) return WAITLIST_FORM_URL;
  const params = new URLSearchParams({ usp: "pp_url", [WAITLIST_PLAN_FIELD]: WAITLIST_PLAN_CHOICES[plan] });
  return `${WAITLIST_FORM_URL}?${params}`;
}

export const WAITLIST_URL = waitlistUrl();
export const VPC_URL = waitlistUrl("vpc");

// No trailing slash anywhere, the root included, so every canonical and sitemap <loc> agree.
export function absoluteUrl(path: string): string {
  const clean = path.replace(/^\/+|\/+$/g, "");
  return clean ? `${SITE_URL}/${clean}` : SITE_URL;
}

export function siteHref(path: string, base = process.env.SITE_BASE ?? ""): string {
  return base + path;
}
