import { GITHUB_URL, INSTALL_CMD } from "@/lib/site";

export type Os = "macos" | "linux" | "windows";
export type Method = "script" | "source" | "zip";
export type App = "copilot" | "vscode" | "claude-code" | "codex";

export type InstallOptions = {
  os: Os;
  method: Method;
  apps: App[];
  preview: boolean;
  hook: boolean;
};

// commands is empty for a step you do by hand, such as a download.
export type InstallStep = { id: string; text: string; commands: string[] };

// The CLI's order (setup.rs ORDER). --harness lists the apps in it.
export const APPS: App[] = ["copilot", "vscode", "claude-code", "codex"];

export const APP_LABELS: Record<App, string> = {
  copilot: "Copilot CLI",
  vscode: "VS Code",
  "claude-code": "Claude Code",
  codex: "Codex",
};

export const OS_LABELS: Record<Os, string> = {
  macos: "macOS",
  linux: "Linux",
  windows: "Windows",
};

export const METHOD_LABELS: Record<Method, string> = {
  script: "Install script",
  source: "From source",
  zip: "Release zip",
};

export const OSES: Os[] = ["macos", "linux", "windows"];

// The apps that load memory when a chat starts. --no-hook only means something for them.
export const HOOK_APPS: App[] = ["claude-code", "codex"];

export const RELEASES_URL = `${GITHUB_URL}/releases/latest`;

// No apps picked means plain `atticdb setup`, which finds the apps on this machine.
export const DEFAULT_OPTIONS: InstallOptions = {
  os: "macos",
  method: "script",
  apps: [],
  preview: true,
  hook: true,
};

const ZIP = "atticdb-x86_64-pc-windows-msvc.zip";
export const PATH_CMD = 'export PATH="$HOME/.local/bin:$PATH"';
export const PATH_NOTE =
  'If your shell says "command not found", run ~/.local/bin/atticdb setup. To fix it for good, add this line to ~/.zshrc (the macOS default) or ~/.bashrc.';
const SOURCE_CMD = "cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked";

export function methodsFor(os: Os): Method[] {
  return os === "windows" ? ["zip", "source"] : ["script", "source"];
}

export function normalize(o: InstallOptions): InstallOptions {
  const methods = methodsFor(o.os);
  return {
    ...o,
    method: methods.includes(o.method) ? o.method : methods[0],
    apps: APPS.filter((app) => o.apps.includes(app)),
  };
}

function installSteps(method: Method): InstallStep[] {
  switch (method) {
    case "script":
      return [
        {
          id: "install",
          text: "Install the binary. It checks the download against SHA256SUMS and never uses sudo.",
          commands: [INSTALL_CMD],
        },
        {
          id: "path",
          text: PATH_NOTE,
          commands: [PATH_CMD],
        },
      ];
    case "source":
      return [
        {
          id: "install",
          text: "Build from source with Rust installed. It lands in ~/.cargo/bin.",
          commands: [SOURCE_CMD],
        },
      ];
    case "zip":
      return [
        {
          id: "download",
          text: `Download ${ZIP} and SHA256SUMS from the latest release.`,
          commands: [],
        },
        {
          id: "install",
          text: "In PowerShell, check the zip, unpack it and put it on your PATH.",
          commands: [
            `(Get-FileHash .\\${ZIP} -Algorithm SHA256).Hash.ToLower()`,
            `Expand-Archive .\\${ZIP} "$env:LOCALAPPDATA\\Programs\\atticdb"`,
            '$env:PATH += ";$env:LOCALAPPDATA\\Programs\\atticdb"',
          ],
        },
      ];
  }
}

// Hook apps in the plan: the picked ones, or any app setup may find when none is picked.
export function hookApplies(o: InstallOptions): boolean {
  return o.apps.length === 0 || o.apps.some((app) => HOOK_APPS.includes(app));
}

export function buildInstallSteps(options: InstallOptions): InstallStep[] {
  const o = normalize(options);
  const steps = installSteps(o.method);
  const picked = o.apps.length > 0;
  const harness = picked ? ` --harness ${o.apps.join(",")}` : "";
  const hookFlag = !o.hook && hookApplies(o) ? " --no-hook" : "";
  if (o.preview) {
    steps.push({
      id: "preview",
      text: "See what setup would change. It writes nothing.",
      commands: [`atticdb setup${harness} --dry-run${hookFlag}`],
    });
  }
  steps.push(
    {
      id: "setup",
      text: picked
        ? "Connect your apps. Every edited file is backed up first."
        : "Connect your apps. Setup finds them and asks which to use. Every edited file is backed up first.",
      commands: [`atticdb setup${harness}${hookFlag}`],
    },
    {
      id: "doctor",
      text: "Check the install. It writes nothing.",
      commands: [`atticdb doctor${harness}`],
    },
  );
  return steps;
}

// Real user agents spell these with capitals. jsdom's "(linux)" stays the default.
export function detectOs(ua: string, platform = ""): Os {
  if (/Windows|Win32|Win64/.test(ua) || /^Win/.test(platform)) return "windows";
  if (/Android|Linux|X11|CrOS/.test(ua) || /Linux/.test(platform)) return "linux";
  return "macos";
}
