export type Credit = {
  title: string;
  creator: string;
  licence: string;
  licenceUrl: string;
  source: string;
  file: string;
};

// From assets.mjs --get, Wikimedia Commons, seen 2026-10-31. Files are resized copies.
export const CREDITS: Credit[] = [
  {
    title: "IBM 350 RAMAC disk mechanism",
    creator: "ArnoldReinhold",
    licence: "CC BY-SA 3.0",
    licenceUrl: "https://creativecommons.org/licenses/by-sa/3.0",
    source: "https://commons.wikimedia.org/wiki/File:IBM_350_RAMAC_disk_mechanism.agr.jpg",
    file: "/images/ibm-350-ramac-disk.jpg",
  },
  {
    title: "Titan Missile Core Memory Board",
    creator: "Steve Jurvetson",
    licence: "CC BY 2.0",
    licenceUrl: "https://creativecommons.org/licenses/by/2.0",
    source:
      "https://commons.wikimedia.org/wiki/File:Titan_Missile_Core_Memory_Board_(49570705568).jpg",
    file: "/images/titan-core-memory-board.jpg",
  },
  {
    title: "1959 NCR 304 Computer Magnetic Tape Reel",
    creator: "Mark Greenia",
    licence: "CC BY-SA 4.0",
    licenceUrl: "https://creativecommons.org/licenses/by-sa/4.0",
    source:
      "https://commons.wikimedia.org/wiki/File:1959_NCR_304_Computer_Magnetic_Tape_Reel.jpg",
    file: "/images/ncr-304-tape-reel.jpg",
  },
  {
    title: "PaperTapes-5and8Hole",
    creator: "TedColes",
    licence: "Public domain",
    licenceUrl: "https://creativecommons.org/publicdomain/mark/1.0/",
    source: "https://commons.wikimedia.org/wiki/File:PaperTapes-5and8Hole.jpg",
    file: "/images/punched-paper-tape.jpg",
  },
];

export function creditFor(file: string): Credit {
  const credit = CREDITS.find((c) => c.file === file);
  if (!credit) throw new Error(`No credit recorded for ${file}`);
  return credit;
}
