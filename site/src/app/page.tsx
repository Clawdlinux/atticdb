import JsonLd from "@/components/JsonLd";
import ClosingCta from "@/components/home/ClosingCta";
import Faq from "@/components/home/Faq";
import Hero from "@/components/home/Hero";
import HowItWorks from "@/components/home/HowItWorks";
import Intro from "@/components/home/Intro";
import Numbers from "@/components/home/Numbers";
import PricingTeaser from "@/components/home/PricingTeaser";
import Showcase from "@/components/home/Showcase";
import WorksWith from "@/components/home/WorksWith";
import { HOME_FAQ } from "@/content/faq";
import { faqJsonLd, pageMetadata, softwareAppJsonLd } from "@/lib/seo";

export const metadata = pageMetadata("/");

export default function Home() {
  return (
    <>
      <JsonLd data={softwareAppJsonLd()} />
      <JsonLd data={faqJsonLd(HOME_FAQ)} />
      <Hero />
      <Intro />
      <Showcase />
      <HowItWorks />
      <WorksWith />
      <Numbers />
      <PricingTeaser />
      <Faq />
      <ClosingCta />
    </>
  );
}
