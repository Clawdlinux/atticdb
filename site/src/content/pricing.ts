import { VPC_URL, WAITLIST_URL } from "@/lib/site";

export type Plan = {
  name: string;
  price: string;
  unit: string;
  status: "Available" | "Planned";
  includes: string[];
} & (
  | { id: "local"; cta: { label: string; href?: never } }
  | { id: "pro" | "team" | "vpc"; cta: { label: string; href: string } }
);

export const PLANS: Plan[] = [
  {
    id: "local",
    name: "Local",
    price: "$0",
    unit: "forever",
    status: "Available",
    includes: [
      "Everything in the binary: sessions, facts, search, MCP, setup for 4 apps, secret redaction, studio.",
      "Apache-2.0 or MIT",
    ],
    cta: { label: "Install AtticDB" },
  },
  {
    id: "pro",
    name: "Cloud Pro",
    price: "$15",
    unit: "per month",
    status: "Planned",
    includes: [
      "1 person, all your machines.",
      "10 GB of synced memory.",
      "Daily snapshots, 30-day restore.",
    ],
    cta: { label: "Join the cloud waitlist", href: WAITLIST_URL },
  },
  {
    id: "team",
    name: "Cloud Team",
    price: "$29",
    unit: "per seat per month",
    status: "Planned",
    includes: [
      "Shared team memory.",
      "Per-agent tokens and roles.",
      "25 GB per seat, pooled.",
    ],
    cta: { label: "Join the cloud waitlist", href: WAITLIST_URL },
  },
  {
    id: "vpc",
    name: "Your cloud",
    price: "Talk to us",
    unit: "",
    status: "Planned",
    includes: ["Runs in your VPC or air-gapped.", "SAML SSO.", "Support SLA"],
    cta: { label: "Talk to us", href: VPC_URL },
  },
];

export const PLAN_FEATURES: { feature: string; values: Record<Plan["id"], string> }[] = [
  {
    feature: "Status",
    values: Object.fromEntries(PLANS.map((plan) => [plan.id, plan.status])) as Record<Plan["id"], string>,
  },
  {
    feature: "Price",
    values: { local: "$0 forever", pro: "$15 per month", team: "$29 per seat per month", vpc: "Talk to us" },
  },
  {
    feature: "Where it runs",
    values: { local: "Your machine", pro: "Cloud", team: "Cloud", vpc: "Your VPC or air-gapped" },
  },
  {
    feature: "Sessions and facts",
    values: { local: "Yes", pro: "Yes", team: "Yes", vpc: "Yes" },
  },
  {
    feature: "MCP tools",
    values: { local: "6 tools", pro: "Same API and MCP", team: "Same API and MCP", vpc: "Same API and MCP" },
  },
  {
    feature: "Sync across machines",
    values: { local: "No", pro: "All your machines", team: "Shared team memory", vpc: "Across your network" },
  },
  {
    feature: "Storage",
    values: { local: "Your disk", pro: "10 GB", team: "25 GB per seat, pooled", vpc: "Your hardware" },
  },
  {
    feature: "Backups",
    values: { local: "You manage backups", pro: "Daily snapshots, 30-day restore", team: "Daily snapshots, 30-day restore", vpc: "Your backup policy" },
  },
  {
    feature: "Team sharing",
    values: { local: "No cloud sharing", pro: "1 person", team: "Shared team memory", vpc: "Shared team memory" },
  },
  {
    feature: "SSO",
    values: { local: "Not included", pro: "Not included", team: "Not included", vpc: "SAML SSO" },
  },
  {
    feature: "Per-agent tokens",
    values: { local: "No", pro: "No", team: "Included", vpc: "Talk to us" },
  },
  {
    feature: "Roles",
    values: { local: "No", pro: "No", team: "Included", vpc: "Talk to us" },
  },
  {
    feature: "GitHub and Google sign-in",
    values: { local: "No", pro: "No", team: "Included", vpc: "Talk to us" },
  },
  {
    feature: "Audit log",
    values: { local: "No", pro: "No", team: "Included", vpc: "Talk to us" },
  },
  {
    feature: "Support SLA",
    values: { local: "No", pro: "No", team: "No", vpc: "Included" },
  },
];
