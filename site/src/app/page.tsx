import JsonLd from "@/components/JsonLd";
import Demo from "@/components/home/Demo";
import Hero from "@/components/home/Hero";
import InstallSection from "@/components/home/InstallSection";
import Numbers from "@/components/home/Numbers";
import PricingTeaser from "@/components/home/PricingTeaser";
import { HOME_FAQ } from "@/content/faq";
import { faqJsonLd, pageMetadata, softwareAppJsonLd } from "@/lib/seo";

export const metadata = pageMetadata("/");

export default function Home() {
  return (
    <>
      <JsonLd data={softwareAppJsonLd()} />
      {/* The home questions render inside the install section. */}
      <JsonLd data={faqJsonLd(HOME_FAQ)} />
      <Hero afterHonest={<Demo />} />
      <InstallSection />
      <Numbers />
      <PricingTeaser />
    </>
  );
}
