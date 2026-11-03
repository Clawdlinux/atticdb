import AgentPrompt from "@/components/AgentPrompt";
import Installer from "@/components/Installer";
import { docMetadata } from "@/lib/seo";
import DocView from "../DocView";

export const metadata = docMetadata("install");

// Its own route: the installer and its motion code load here and on no other docs page.
// The agent prompt comes first. The installer's Copy all keeps the page's one accent.
export default function DocsInstallPage() {
  return (
    <DocView slug="install">
      <AgentPrompt tone="ink" />
      <p>Or pick the commands yourself.</p>
      <Installer />
    </DocView>
  );
}
