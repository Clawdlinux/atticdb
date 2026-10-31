export const SITE_URL = "https://atticdb.com";
export const GITHUB_URL = "https://github.com/Clawdlinux/atticdb";
export const DOCS_URL = GITHUB_URL + "#readme";
export const INSTALL_CMD =
  "curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh";
export const WAITLIST_URL =
  "https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20waitlist%3A%20";
export const VPC_URL =
  "https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20in%20my%20VPC%3A%20";

// No trailing slash anywhere, the root included, so every canonical and sitemap <loc> agree.
export function absoluteUrl(path: string): string {
  const clean = path.replace(/^\/+|\/+$/g, "");
  return clean ? `${SITE_URL}/${clean}` : SITE_URL;
}
