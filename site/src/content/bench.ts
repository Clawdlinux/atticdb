export type Disk = "nvme" | "premium-v2";
export type System = "atticdb" | "redis" | "valkey" | "dicedb" | "sqlite";
export type BenchRow = {
  disk: Disk;
  load: string;
  writers: number;
  thinkMs: number;
  system: System;
  acksPerSec: number;
  p50us: number;
  p99us: number;
  p99RangeUs: [number, number];
  p999us: number;
  maxUs: number;
};

export const BENCH_META = {
  date: "2026-10-24",
  machine: "Azure Standard_L8s_v3",
  reps: 7,
};

// Medians from bench/H2H.md. Latencies are microseconds, including the p99 ranges.
export const BENCH_ROWS: BenchRow[] = [
  {
    disk: "premium-v2", load: "closed", writers: 1, thinkMs: 0, system: "atticdb",
    acksPerSec: 1167, p50us: 817, p99us: 1514,
    p99RangeUs: [1509, 1636], p999us: 2097, maxUs: 2590,
  },
  {
    disk: "premium-v2", load: "closed", writers: 1, thinkMs: 0, system: "redis",
    acksPerSec: 466, p50us: 2136, p99us: 3231,
    p99RangeUs: [3079, 3253], p999us: 4246, maxUs: 11650,
  },
  {
    disk: "premium-v2", load: "closed", writers: 1, thinkMs: 0, system: "valkey",
    acksPerSec: 472, p50us: 2131, p99us: 3214,
    p99RangeUs: [3057, 3278], p999us: 4215, maxUs: 8883,
  },
  {
    disk: "premium-v2", load: "closed", writers: 1, thinkMs: 0, system: "dicedb",
    acksPerSec: 473, p50us: 2128, p99us: 3204,
    p99RangeUs: [3133, 3250], p999us: 4222, maxUs: 7982,
  },
  {
    disk: "premium-v2", load: "closed", writers: 1, thinkMs: 0, system: "sqlite",
    acksPerSec: 1143, p50us: 735, p99us: 2365,
    p99RangeUs: [2253, 2479], p999us: 3029, maxUs: 4763,
  },
  {
    disk: "premium-v2", load: "closed", writers: 4, thinkMs: 0, system: "atticdb",
    acksPerSec: 2619, p50us: 1473, p99us: 2366,
    p99RangeUs: [2246, 2404], p999us: 2875, maxUs: 4026,
  },
  {
    disk: "premium-v2", load: "closed", writers: 4, thinkMs: 0, system: "redis",
    acksPerSec: 1022, p50us: 3972, p99us: 5568,
    p99RangeUs: [5436, 5646], p999us: 6945, maxUs: 13518,
  },
  {
    disk: "premium-v2", load: "closed", writers: 4, thinkMs: 0, system: "valkey",
    acksPerSec: 936, p50us: 4255, p99us: 5622,
    p99RangeUs: [5467, 5738], p999us: 7943, maxUs: 15345,
  },
  {
    disk: "premium-v2", load: "closed", writers: 4, thinkMs: 0, system: "dicedb",
    acksPerSec: 932, p50us: 4273, p99us: 5691,
    p99RangeUs: [5418, 5810], p999us: 8424, maxUs: 16121,
  },
  {
    disk: "premium-v2", load: "closed", writers: 4, thinkMs: 0, system: "sqlite",
    acksPerSec: 1342, p50us: 704, p99us: 1960,
    p99RangeUs: [1807, 1993], p999us: 2716, maxUs: 9944485,
  },
  {
    disk: "premium-v2", load: "closed", writers: 16, thinkMs: 0, system: "atticdb",
    acksPerSec: 9328, p50us: 1618, p99us: 2734,
    p99RangeUs: [2394, 2937], p999us: 3323, maxUs: 4173,
  },
  {
    disk: "premium-v2", load: "closed", writers: 16, thinkMs: 0, system: "redis",
    acksPerSec: 5151, p50us: 3292, p99us: 5195,
    p99RangeUs: [5016, 5582], p999us: 7465, maxUs: 16279,
  },
  {
    disk: "premium-v2", load: "closed", writers: 16, thinkMs: 0, system: "valkey",
    acksPerSec: 5096, p50us: 3185, p99us: 5161,
    p99RangeUs: [5038, 5634], p999us: 6327, maxUs: 25764,
  },
  {
    disk: "premium-v2", load: "closed", writers: 16, thinkMs: 0, system: "dicedb",
    acksPerSec: 5086, p50us: 3513, p99us: 5063,
    p99RangeUs: [4988, 5785], p999us: 6072, maxUs: 11252,
  },
  {
    disk: "premium-v2", load: "closed", writers: 16, thinkMs: 0, system: "sqlite",
    acksPerSec: 1421, p50us: 704, p99us: 1696,
    p99RangeUs: [1639, 1839], p999us: 3098, maxUs: 46787988,
  },
  {
    disk: "premium-v2", load: "agent", writers: 64, thinkMs: 5, system: "atticdb",
    acksPerSec: 9664, p50us: 1286, p99us: 2246,
    p99RangeUs: [2194, 2380], p999us: 2764, maxUs: 6213,
  },
  {
    disk: "premium-v2", load: "agent", writers: 64, thinkMs: 5, system: "redis",
    acksPerSec: 7779, p50us: 2916, p99us: 4693,
    p99RangeUs: [4526, 5059], p999us: 6545, maxUs: 13334,
  },
  {
    disk: "premium-v2", load: "agent", writers: 64, thinkMs: 5, system: "valkey",
    acksPerSec: 7796, p50us: 2901, p99us: 4597,
    p99RangeUs: [4442, 5139], p999us: 6992, maxUs: 11483,
  },
  {
    disk: "premium-v2", load: "agent", writers: 64, thinkMs: 5, system: "dicedb",
    acksPerSec: 7840, p50us: 2807, p99us: 4592,
    p99RangeUs: [4424, 4968], p999us: 6998, maxUs: 11928,
  },
  {
    disk: "premium-v2", load: "agent", writers: 64, thinkMs: 5, system: "sqlite",
    acksPerSec: 1105, p50us: 3860, p99us: 830031,
    p99RangeUs: [829880, 830422], p999us: 1730934, maxUs: 3731498,
  },
  {
    disk: "premium-v2", load: "agent", writers: 256, thinkMs: 50, system: "atticdb",
    acksPerSec: 4270, p50us: 1284, p99us: 2259,
    p99RangeUs: [2184, 2392], p999us: 2827, maxUs: 5302,
  },
  {
    disk: "premium-v2", load: "agent", writers: 256, thinkMs: 50, system: "redis",
    acksPerSec: 4151, p50us: 3070, p99us: 4955,
    p99RangeUs: [4700, 5433], p999us: 12560, maxUs: 33249,
  },
  {
    disk: "premium-v2", load: "agent", writers: 256, thinkMs: 50, system: "valkey",
    acksPerSec: 4154, p50us: 3072, p99us: 4860,
    p99RangeUs: [4694, 9744], p999us: 8071, maxUs: 28153,
  },
  {
    disk: "premium-v2", load: "agent", writers: 256, thinkMs: 50, system: "dicedb",
    acksPerSec: 4149, p50us: 3065, p99us: 4985,
    p99RangeUs: [4590, 5365], p999us: 9384, maxUs: 27769,
  },
  {
    disk: "premium-v2", load: "agent", writers: 256, thinkMs: 50, system: "sqlite",
    acksPerSec: 1038, p50us: 9405, p99us: 2130899,
    p99RangeUs: [1830926, 2131252], p999us: 3732598, maxUs: 6534968,
  },
  {
    disk: "nvme", load: "closed", writers: 1, thinkMs: 0, system: "atticdb",
    acksPerSec: 9799, p50us: 94, p99us: 128,
    p99RangeUs: [113, 320], p999us: 145, maxUs: 300,
  },
  {
    disk: "nvme", load: "closed", writers: 1, thinkMs: 0, system: "redis",
    acksPerSec: 8110, p50us: 116, p99us: 184,
    p99RangeUs: [171, 708], p999us: 216, maxUs: 354,
  },
  {
    disk: "nvme", load: "closed", writers: 1, thinkMs: 0, system: "valkey",
    acksPerSec: 8434, p50us: 108, p99us: 172,
    p99RangeUs: [166, 602], p999us: 212, maxUs: 330,
  },
  {
    disk: "nvme", load: "closed", writers: 1, thinkMs: 0, system: "dicedb",
    acksPerSec: 8436, p50us: 110, p99us: 179,
    p99RangeUs: [166, 186], p999us: 214, maxUs: 344,
  },
  {
    disk: "nvme", load: "closed", writers: 1, thinkMs: 0, system: "sqlite",
    acksPerSec: 19410, p50us: 32, p99us: 157,
    p99RangeUs: [152, 170], p999us: 411, maxUs: 736,
  },
  {
    disk: "nvme", load: "closed", writers: 4, thinkMs: 0, system: "atticdb",
    acksPerSec: 24516, p50us: 146, p99us: 214,
    p99RangeUs: [213, 218], p999us: 251, maxUs: 438,
  },
  {
    disk: "nvme", load: "closed", writers: 4, thinkMs: 0, system: "redis",
    acksPerSec: 17504, p50us: 221, p99us: 304,
    p99RangeUs: [296, 317], p999us: 348, maxUs: 532,
  },
  {
    disk: "nvme", load: "closed", writers: 4, thinkMs: 0, system: "valkey",
    acksPerSec: 18444, p50us: 202, p99us: 298,
    p99RangeUs: [286, 324], p999us: 344, maxUs: 474,
  },
  {
    disk: "nvme", load: "closed", writers: 4, thinkMs: 0, system: "dicedb",
    acksPerSec: 18149, p50us: 208, p99us: 310,
    p99RangeUs: [298, 327], p999us: 361, maxUs: 467,
  },
  {
    disk: "nvme", load: "closed", writers: 4, thinkMs: 0, system: "sqlite",
    acksPerSec: 22162, p50us: 32, p99us: 143,
    p99RangeUs: [138, 160], p999us: 622, maxUs: 729400,
  },
  {
    disk: "nvme", load: "closed", writers: 16, thinkMs: 0, system: "atticdb",
    acksPerSec: 48611, p50us: 305, p99us: 502,
    p99RangeUs: [493, 519], p999us: 674, maxUs: 1210,
  },
  {
    disk: "nvme", load: "closed", writers: 16, thinkMs: 0, system: "redis",
    acksPerSec: 44572, p50us: 299, p99us: 549,
    p99RangeUs: [543, 564], p999us: 604, maxUs: 932,
  },
  {
    disk: "nvme", load: "closed", writers: 16, thinkMs: 0, system: "valkey",
    acksPerSec: 42928, p50us: 311, p99us: 573,
    p99RangeUs: [546, 580], p999us: 624, maxUs: 810,
  },
  {
    disk: "nvme", load: "closed", writers: 16, thinkMs: 0, system: "dicedb",
    acksPerSec: 44693, p50us: 300, p99us: 547,
    p99RangeUs: [537, 572], p999us: 609, maxUs: 947,
  },
  {
    disk: "nvme", load: "closed", writers: 16, thinkMs: 0, system: "sqlite",
    acksPerSec: 24936, p50us: 32, p99us: 124,
    p99RangeUs: [123, 128], p999us: 648, maxUs: 2831276,
  },
  {
    disk: "nvme", load: "agent", writers: 64, thinkMs: 5, system: "atticdb",
    acksPerSec: 11687, p50us: 148, p99us: 416,
    p99RangeUs: [273, 521], p999us: 786, maxUs: 4871,
  },
  {
    disk: "nvme", load: "agent", writers: 64, thinkMs: 5, system: "redis",
    acksPerSec: 11648, p50us: 183, p99us: 382,
    p99RangeUs: [336, 567], p999us: 964, maxUs: 5317,
  },
  {
    disk: "nvme", load: "agent", writers: 64, thinkMs: 5, system: "valkey",
    acksPerSec: 11641, p50us: 184, p99us: 506,
    p99RangeUs: [341, 564], p999us: 1053, maxUs: 6766,
  },
  {
    disk: "nvme", load: "agent", writers: 64, thinkMs: 5, system: "dicedb",
    acksPerSec: 11644, p50us: 182, p99us: 504,
    p99RangeUs: [334, 547], p999us: 1029, maxUs: 5292,
  },
  {
    disk: "nvme", load: "agent", writers: 64, thinkMs: 5, system: "sqlite",
    acksPerSec: 8531, p50us: 57, p99us: 33381,
    p99RangeUs: [33367, 33414], p999us: 103539, maxUs: 428896,
  },
  {
    disk: "nvme", load: "agent", writers: 256, thinkMs: 50, system: "atticdb",
    acksPerSec: 4351, p50us: 138, p99us: 558,
    p99RangeUs: [225, 636], p999us: 736, maxUs: 8698,
  },
  {
    disk: "nvme", load: "agent", writers: 256, thinkMs: 50, system: "redis",
    acksPerSec: 4351, p50us: 145, p99us: 612,
    p99RangeUs: [292, 752], p999us: 2185, maxUs: 7625,
  },
  {
    disk: "nvme", load: "agent", writers: 256, thinkMs: 50, system: "valkey",
    acksPerSec: 4352, p50us: 142, p99us: 496,
    p99RangeUs: [293, 1318], p999us: 2758, maxUs: 8039,
  },
  {
    disk: "nvme", load: "agent", writers: 256, thinkMs: 50, system: "dicedb",
    acksPerSec: 4352, p50us: 140, p99us: 348,
    p99RangeUs: [289, 1078], p999us: 2103, maxUs: 7392,
  },
  {
    disk: "nvme", load: "agent", writers: 256, thinkMs: 50, system: "sqlite",
    acksPerSec: 4242, p50us: 68, p99us: 18489,
    p99RangeUs: [18354, 18732], p999us: 103782, maxUs: 329060,
  },
];

export function ackFor(disk: Disk, writers: 1 | 4 | 16): { p50us: number; acksPerSec: number } {
  const row = BENCH_ROWS.find(
    (row) => row.disk === disk && row.writers === writers
      && row.load === "closed" && row.system === "atticdb",
  );
  if (!row) throw new Error("Missing closed-loop benchmark row.");
  return { p50us: row.p50us, acksPerSec: row.acksPerSec };
}
