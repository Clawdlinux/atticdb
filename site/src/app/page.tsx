import JsonLd from "@/components/JsonLd";
import Hero from "@/components/home/Hero";
import InstallSection from "@/components/home/InstallSection";
import Numbers from "@/components/home/Numbers";
import PricingTeaser from "@/components/home/PricingTeaser";
import { pageMetadata, softwareAppJsonLd } from "@/lib/seo";

export const metadata = pageMetadata("/");

export default function Home() {
  return (
    <>
      <JsonLd data={softwareAppJsonLd()} />
      <Hero />
      <InstallSection />
      <Numbers />
      <PricingTeaser />
    </>
  );
}
