"""Draws bench/h2h-*.svg from bench/h2h-*.tsv. Standard library only.

Usage: python3 bench/h2h_chart.py
"""

import collections
import pathlib
import statistics

HERE = pathlib.Path(__file__).parent
SYSTEMS = ["atticdb", "redis", "valkey", "dicedb", "sqlite"]
COLORS = {"atticdb": "#e4572e", "redis": "#5b6c8f", "valkey": "#8aa1c7",
          "dicedb": "#b8c4dd", "sqlite": "#76b041"}
DISKS = [("premium-v2", "Premium SSD v2"), ("nvme", "Local NVMe")]
METRICS = [(0, "Durable acks per second (higher is better)"),
           (2, "p99 ack latency, µs (lower is better)")]


def medians(path):
    runs = collections.defaultdict(list)
    for line in path.read_text().splitlines():
        if line.startswith(("#", "system")):
            continue
        system, writers, _, *values = line.split("\t")
        runs[(int(writers), system)].append([float(v) for v in values])
    reps = min(len(rows) for rows in runs.values())
    return {key: [statistics.median(c) for c in zip(*rows)] for key, rows in runs.items()}, reps


def panel(x, y, w, h, title, data, metric):
    writers = sorted({k[0] for k in data})
    top = max(data[(n, s)][metric] for n in writers for s in SYSTEMS) * 1.15
    out = [f'<text x="{x}" y="{y - 12}" font-size="14" font-weight="600">{title}</text>',
           f'<line x1="{x}" y1="{y + h}" x2="{x + w}" y2="{y + h}" stroke="#999"/>']
    group = w / len(writers)
    bar = group * 0.8 / len(SYSTEMS)
    for i, n in enumerate(writers):
        gx = x + i * group + group * 0.1
        out.append(f'<text x="{gx + group * 0.4}" y="{y + h + 16}" font-size="11" '
                   f'text-anchor="middle">{n} writer{"s" * (n > 1)}</text>')
        for j, system in enumerate(SYSTEMS):
            value = data[(n, system)][metric]
            bh = h * value / top
            bx, by = gx + j * bar, y + h - bh
            label = f"{value:,.0f}"
            out.append(f'<rect x="{bx:.1f}" y="{by:.1f}" width="{bar * 0.9:.1f}" '
                       f'height="{bh:.1f}" fill="{COLORS[system]}"/>')
            out.append(f'<text x="{bx + bar * 0.45:.1f}" y="{by - 3:.1f}" font-size="8" '
                       f'text-anchor="middle">{label}</text>')
    return out


def main():
    w, h, pad = 420, 220, 60
    width, height = pad + 2 * (w + pad), 2 * (h + 2 * pad) + 100
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
             f'font-family="Helvetica, Arial, sans-serif">',
             f'<rect width="{width}" height="{height}" fill="white"/>',
             f'<text x="{pad}" y="28" font-size="17" font-weight="700">'
             'Durable 256-byte appends, ack after fsync, closed loop, 1 client for every server</text>']
    stalls, reps = [], set()
    for col, (disk, name) in enumerate(DISKS):
        data, n = medians(HERE / f"h2h-{disk}.tsv")
        reps.add(n)
        stalls.append(f"{data[(16, 'sqlite')][4] / 1e6:.1f} s on {name}")
        for row, (metric, label) in enumerate(METRICS):
            x, y = pad + col * (w + pad), 80 + row * (h + 2 * pad)
            parts += panel(x, y, w, h, f"{name}: {label}", data, metric)
    parts.append(f'<text x="{pad}" y="{height - 46}" font-size="12" fill="#444">'
                 f"SQLite p99 hides lock stalls. Its slowest ack at 16 writers took "
                 f"{' and '.join(stalls)}. Medians of {min(reps)} interleaved reps, Azure L8s_v3.</text>")
    lx = pad
    for system in SYSTEMS:
        parts.append(f'<rect x="{lx}" y="{height - 26}" width="12" height="12" '
                     f'fill="{COLORS[system]}"/>')
        parts.append(f'<text x="{lx + 16}" y="{height - 16}" font-size="12">{system}</text>')
        lx += 90
    parts.append("</svg>")
    (HERE / "h2h.svg").write_text("\n".join(parts) + "\n")


if __name__ == "__main__":
    main()
