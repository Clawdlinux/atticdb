import { BENCH_ROWS, type BenchRow, type Disk, type System } from "@/content/bench";

const fmt = new Intl.NumberFormat("en-US");

const SYSTEMS: { id: System; label: string; fill: string; opacity: number }[] = [
  { id: "atticdb", label: "AtticDB", fill: "var(--accent)", opacity: 1 },
  { id: "redis", label: "Redis", fill: "var(--ink)", opacity: 0.78 },
  { id: "valkey", label: "Valkey", fill: "var(--ink)", opacity: 0.62 },
  { id: "dicedb", label: "DiceDB", fill: "var(--ink)", opacity: 0.48 },
  { id: "sqlite", label: "SQLite", fill: "var(--ink)", opacity: 0.34 },
];

const WRITER_GROUPS = [1, 4, 16] as const;

const DISK_LABEL: Record<Disk, string> = {
  "premium-v2": "Premium SSD v2",
  nvme: "Local NVMe",
};

const SYSTEM_LABEL: Record<System, string> = {
  atticdb: "AtticDB",
  redis: "Redis",
  valkey: "Valkey",
  dicedb: "DiceDB",
  sqlite: "SQLite",
};

function findClosedRow(disk: Disk, writers: number, system: System): BenchRow {
  const row = BENCH_ROWS.find(
    (r) => r.disk === disk && r.load === "closed" && r.writers === writers && r.system === system,
  );
  if (!row) {
    throw new Error(`Missing benchmark row for ${disk} writers=${writers} system=${system}`);
  }
  return row;
}

function buildAriaLabel(disk: Disk): string {
  const parts = WRITER_GROUPS.map((writers) => {
    const attic = findClosedRow(disk, writers, "atticdb");
    const others = SYSTEMS.filter((s) => s.id !== "atticdb").map((s) =>
      findClosedRow(disk, writers, s.id),
    );
    const bestOther = others.reduce((best, cur) =>
      cur.acksPerSec > best.acksPerSec ? cur : best,
    );
    const wLabel = writers === 1 ? "1 writer" : `${writers} writers`;
    return `${wLabel} AtticDB ${fmt.format(attic.acksPerSec)} vs ${SYSTEM_LABEL[bestOther.system]} ${fmt.format(bestOther.acksPerSec)}`;
  });
  return `Closed-loop durable acks per second on ${DISK_LABEL[disk]}: ${parts.join("; ")}.`;
}

export default function BenchChart({ disk }: { disk: Disk }) {
  const rows = BENCH_ROWS.filter((r) => r.disk === disk && r.load === "closed");
  const maxAcks = Math.max(...rows.map((r) => r.acksPerSec));
  const ariaLabel = buildAriaLabel(disk);

  const svgWidth = 780;
  const svgHeight = 340;
  const topY = 66;
  const baseY = 272;
  const maxBarH = baseY - topY;
  const groupWidth = 232;
  const groupGap = 42;
  const barWidth = 38;
  const barGap = 10;

  return (
    <div>
      <svg
        viewBox={`0 0 ${svgWidth} ${svgHeight}`}
        role="img"
        aria-label={ariaLabel}
        style={{
          width: "100%",
          height: "auto",
          fontFamily: "var(--font-text), Georgia, serif",
          fontVariantNumeric: "tabular-nums lining-nums",
        }}
      >
        <g transform="translate(0, 22)">
          {SYSTEMS.map((sys, idx) => {
            const x = idx * 140;
            return (
              <g key={sys.id} transform={`translate(${x}, 0)`}>
                <rect
                  x={0}
                  y={-12}
                  width={14}
                  height={14}
                  fill={sys.fill}
                  fillOpacity={sys.opacity}
                />
                <text x={22} y={0} fill="var(--ink)" fontSize="15">
                  {sys.label}
                </text>
              </g>
            );
          })}
        </g>

        {WRITER_GROUPS.map((writers, groupIdx) => {
          const groupX = groupIdx * (groupWidth + groupGap);
          const groupLabel = writers === 1 ? "1 writer" : `${writers} writers`;
          return (
            <g key={writers} transform={`translate(${groupX}, 0)`}>
              {SYSTEMS.map((sys, barIdx) => {
                const row = findClosedRow(disk, writers, sys.id);
                const rawH = (row.acksPerSec / maxAcks) * maxBarH;
                const barH = Math.max(8, Math.round(rawH));
                const x = barIdx * (barWidth + barGap);
                const y = baseY - barH;
                return (
                  <g key={sys.id}>
                    <rect
                      data-bar={sys.id}
                      x={x}
                      y={y}
                      width={barWidth}
                      height={barH}
                      fill={sys.fill}
                      fillOpacity={sys.opacity}
                    />
                    <text
                      x={x + barWidth / 2}
                      y={y - 8}
                      fill="var(--ink)"
                      fontSize="13.5"
                      textAnchor="middle"
                    >
                      {fmt.format(row.acksPerSec)}
                    </text>
                    <text
                      x={x + barWidth / 2}
                      y={baseY + 22}
                      fill="var(--muted)"
                      fontSize="13.5"
                      textAnchor="middle"
                    >
                      {sys.label}
                    </text>
                  </g>
                );
              })}
              <text
                x={groupWidth / 2}
                y={baseY + 52}
                fill="var(--ink)"
                fontSize="16"
                fontWeight="500"
                textAnchor="middle"
              >
                {groupLabel}
              </text>
            </g>
          );
        })}
      </svg>

      <table className="visually-hidden">
        <caption>{`Closed-loop durable acks per second on ${DISK_LABEL[disk]}`}</caption>
        <thead>
          <tr>
            <th scope="col">Writers</th>
            {SYSTEMS.map((sys) => (
              <th key={sys.id} scope="col">
                {sys.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {WRITER_GROUPS.map((writers) => (
            <tr key={writers}>
              <th scope="row">{writers}</th>
              {SYSTEMS.map((sys) => {
                const row = findClosedRow(disk, writers, sys.id);
                return <td key={sys.id}>{fmt.format(row.acksPerSec)}</td>;
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
