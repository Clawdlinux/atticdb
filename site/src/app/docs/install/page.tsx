import Installer from "@/components/Installer";
import { docMetadata } from "@/lib/seo";
import DocView from "../DocView";

export const metadata = docMetadata("install");

// Its own route: the installer and its motion code load here and on no other docs page.
export default function DocsInstallPage() {
  return (
    <DocView slug="install">
      <Installer />
    </DocView>
  );
}
