import ClosingCta from "@/components/home/ClosingCta";
import Faq from "@/components/home/Faq";
import Hero from "@/components/home/Hero";
import HowItWorks from "@/components/home/HowItWorks";
import Intro from "@/components/home/Intro";
import Numbers from "@/components/home/Numbers";
import PricingTeaser from "@/components/home/PricingTeaser";
import Showcase from "@/components/home/Showcase";
import WorksWith from "@/components/home/WorksWith";

export default function Home() {
  return (
    <>
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
