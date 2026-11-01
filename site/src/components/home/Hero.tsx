import HeroClient from "./HeroClient";
import TapePoster from "./TapePoster";

// The poster is drawn here, on the server, so the sim it comes from never ships in the first load.
export default function Hero() {
  return <HeroClient poster={<TapePoster />} />;
}
