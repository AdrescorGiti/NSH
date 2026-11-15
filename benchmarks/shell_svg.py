#!/usr/bin/env python3
"""Render the README comparison from validated timing samples."""

import argparse
from html import escape
import json
from pathlib import Path
from statistics import median


SHELLS = ("NSH", "Dash", "Zsh", "Fish", "Bash")
WORKLOADS = (
    ("startup_warm", "Warm startup"),
    ("startup_cold", "Cold executable pages"),
    ("builtins", "2,000 true builtins"),
    ("printf", "800 printf calls"),
    ("functions", "1,600 function calls"),
    ("compound", "Multiline block · 1,200 true calls"),
    ("pipeline", "16 MiB pipeline"),
    ("external", "100 /usr/bin/true processes"),
    ("external_path", "100 cat processes via PATH"),
)


def median_ms(report, workload, shell):
    row = report["workloads"][workload][shell]
    if "error" in row or "trace_error" in row or not row["samples"]:
        raise ValueError(f"Missing validated samples for {workload} / {shell}")
    samples = [sample["ns"] for sample in row["samples"]]
    if any(type(ns) is not int or ns <= 0 for ns in samples):
        raise ValueError(f"Invalid timing sample for {workload} / {shell}")
    return median(samples) / 1e6


def render(report):
    title = "Shell benchmarks"
    subtitle = "Median wall-clock time per workload (ms)"
    legend = "Bold values are fastest at the shown precision."
    versions = [
        report["shells"][shell].get("version_number") or report["shells"][shell]["version"]
        for shell in SHELLS
    ]
    rows = [
        (label, [median_ms(report, workload, shell) for shell in SHELLS])
        for workload, label in WORKLOADS
    ]
    env = report["environment"]
    system = f"{env['distribution']} · {env['arch']} · {env['date_utc'][:10]} UTC"
    description = [
        subtitle + ". Lower is faster.",
        system + ".",
        ", ".join(f"{shell} {version}" for shell, version in zip(SHELLS, versions)) + ".",
    ]
    description.extend(
        f"{label} — " + ", ".join(
            f"{shell} {value:.3f} ms" for shell, value in zip(SHELLS, values)
        ) + "."
        for label, values in rows
    )

    margin = 24
    label_width = 304
    cell_width = 120
    table_top = 100
    header_height = 55
    row_height = 36
    table_width = label_width + cell_width * len(SHELLS)
    width = table_width + margin * 2
    height = table_top + header_height + row_height * len(rows) + 54
    columns = [margin + label_width + cell_width * (index + 0.5) for index in range(len(SHELLS))]
    svg = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" xml:lang="en" role="img" aria-labelledby="title description">',
        f'<title id="title">{escape(title)}</title>',
        f'<desc id="description">{escape(" ".join(description))}</desc>',
        '<g aria-hidden="true">',
        f'<rect width="{width}" height="{height}" rx="15" fill="#0c111a"/>',
        '<g font-family="DejaVu Sans, sans-serif">',
    ]

    def text(x, y, value, *, size=14, fill="#e1e9f4", anchor="start", bold=False, mono=False):
        font = ' font-family="DejaVu Sans Mono, monospace"' if mono else ""
        weight = ' font-weight="700"' if bold else ""
        svg.append(
            f'<text x="{x:g}" y="{y}" fill="{fill}" font-size="{size}" '
            f'text-anchor="{anchor}"{font}{weight}>{escape(value)}</text>'
        )

    text(margin, 49, title, size=31, fill="#f5f8ff", bold=True)
    text(margin, 75, subtitle, fill="#acbed4")
    svg.append(
        f'<rect x="{margin}" y="{table_top}" width="{table_width}" height="{header_height}" '
        'rx="7" fill="#1d2a3b"/>'
    )
    text(margin + 16, table_top + 33, "Workload", size=13, bold=True)
    for x, shell, version in zip(columns, SHELLS, versions):
        text(x, table_top + 24, shell, size=15, anchor="middle", bold=True)
        short = version if len(version) <= 16 else version[:15] + "…"
        text(x, table_top + 42, short, size=11, fill="#acbed4", anchor="middle", mono=True)

    for index, (label, values) in enumerate(rows):
        y = table_top + header_height + index * row_height
        fill = "#141e2c" if index % 2 == 0 else "#101925"
        svg.append(
            f'<rect x="{margin}" y="{y}" width="{table_width}" height="{row_height}" fill="{fill}"/>'
        )
        text(margin + 16, y + 23, label, size=14)
        fastest = f"{min(values):.3f}"
        for x, value in zip(columns, values):
            formatted = f"{value:.3f}"
            winner = formatted == fastest
            if winner:
                svg.append(
                    f'<rect x="{x - 48:g}" y="{y + 5}" width="96" height="26" rx="5" fill="#1b3b3b"/>'
                )
            text(
                x, y + 23, formatted, size=14, anchor="middle", bold=winner,
                fill="#a8f2dd" if winner else "#e1e9f4", mono=True,
            )

    text(margin, height - 23, system, size=12, fill="#acbed4")
    text(width - margin, height - 23, legend, size=12, fill="#acbed4", anchor="end")
    svg.extend(["</g>", "</g>", "</svg>"])
    return "\n".join(svg) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    report = json.loads(args.report.read_text(encoding="utf-8"))
    args.output.write_text(render(report), encoding="utf-8")


if __name__ == "__main__":
    main()
