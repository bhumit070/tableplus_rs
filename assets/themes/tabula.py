#!/usr/bin/env python3
"""Writes tabula.json (the Tabula Dark and Tabula Light themes) from the
colour tokens in design.md. Edit the tokens here, then run:
    python3 assets/themes/tabula.py
"""
import json
from pathlib import Path

# Design tokens: (dark, light).
T = {
    "bg/canvas": ("#0B0C0F", "#FFFFFF"),
    "bg/rail": ("#0F1013", "#F4F4F6"),
    "bg/sidebar": ("#121317", "#F8F8FA"),
    "bg/surface": ("#16171C", "#FFFFFF"),
    "bg/raised": ("#1C1E24", "#FFFFFF"),
    "bg/hover": ("#22242B", "#ECEDF1"),
    "bg/zebra": ("#0D0E12", "#FAFAFB"),
    "border/default": ("#24262D", "#E2E3E8"),
    "border/subtle": ("#1D1F25", "#EDEEF1"),
    "text/primary": ("#E8E9ED", "#17181C"),
    "text/secondary": ("#A4A8B3", "#4B4F58"),
    "text/muted": ("#6B707C", "#7E838D"),
    "text/faint": ("#4A4E58", "#A7ABB3"),
    "accent": ("#7C8CFF", "#4F5FE8"),
    "success": ("#3FB97A", "#1F9D5C"),
    "warning": ("#E5A93B", "#B7791F"),
    "danger": ("#EF5A5A", "#D93C3C"),
    "info": ("#4FA3F7", "#2B7BD4"),
    "keyword": ("#B07CFF", "#8A4FE0"),
    "number": ("#3CC7C2", "#1E9E99"),
}

# Hover and pressed shades of the fill colours: (dark, light), each
# (hover, pressed). Lighter in dark mode, darker in light mode.
SHADE = {
    "accent": (("#8E9BFF", "#6A7AF0"), ("#4252D9", "#3846C4")),
    "success": (("#52C68A", "#34A86C"), ("#1A8A51", "#167646")),
    "warning": (("#EBB653", "#D29A2F"), ("#A36B19", "#8F5E16")),
    "danger": (("#F26E6E", "#E04848"), ("#C53434", "#B02E2E")),
    "info": (("#66AFF8", "#3F93E8"), ("#2569BA", "#205BA1")),
}


def alpha(hex_color, a):
    return hex_color + "%02X" % round(a * 255)


def theme(i, name, mode):
    t = {k: v[i] for k, v in T.items()}
    dark = mode == "dark"
    # Text on a coloured fill (primary button, danger button, ...).
    on_fill = t["bg/canvas"] if dark else "#FFFFFF"
    c = {
        "background": t["bg/canvas"],
        "foreground": t["text/primary"],
        "border": t["border/default"],
        "input.border": t["border/default"],
        "ring": t["accent"],
        "caret": t["accent"],
        "accent.background": alpha(t["accent"], 0.16),
        "accent.foreground": t["text/primary"],
        "muted.background": t["bg/hover"],
        "muted.foreground": t["text/muted"],
        "popover.background": t["bg/raised"],
        "popover.foreground": t["text/primary"],
        "secondary.background": t["bg/surface"],
        "secondary.foreground": t["text/secondary"],
        "secondary.hover.background": t["bg/hover"],
        "secondary.active.background": "#2A2C34" if dark else "#E2E3E8",
        "selection.background": alpha(t["accent"], 0.35 if dark else 0.28),
        "overlay": "#0000008C" if dark else "#0000004D",
        "title_bar.background": t["bg/rail"],
        "title_bar.border": t["border/default"],
        "status_bar.background": t["bg/rail"],
        "status_bar.border": t["border/default"],
        "sidebar.background": t["bg/sidebar"],
        "sidebar.foreground": t["text/secondary"],
        "sidebar.border": t["border/default"],
        "sidebar.accent.background": t["bg/hover"],
        "sidebar.accent.foreground": t["text/primary"],
        "sidebar.primary.background": t["accent"],
        "sidebar.primary.foreground": on_fill,
        "tab_bar.background": t["bg/rail"],
        "tab_bar.segmented.background": t["bg/surface"],
        "tab.background": "#00000000",
        "tab.foreground": t["text/secondary"],
        "tab.active.background": t["bg/canvas"],
        "tab.active.foreground": t["text/primary"],
        "table.background": t["bg/canvas"],
        "table.even.background": t["bg/zebra"],
        "table.head.background": t["bg/sidebar"],
        "table.head.foreground": t["text/secondary"],
        "table.foot.background": t["bg/sidebar"],
        "table.foot.foreground": t["text/secondary"],
        # Translucent, so staged-edit tints still show under the hover.
        "table.hover.background": "#FFFFFF0A" if dark else "#0000000A",
        "table.active.background": alpha(t["accent"], 0.12),
        "table.active.border": t["accent"],
        "table.row.border": t["border/subtle"],
        "list.background": "#00000000",
        "list.even.background": t["bg/zebra"],
        "list.head.background": t["bg/sidebar"],
        "list.hover.background": t["bg/hover"],
        "list.active.background": alpha(t["accent"], 0.14),
        "list.active.border": t["accent"],
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": t["text/faint"],
        "scrollbar.thumb.hover.background": t["text/muted"],
        "switch.background": t["border/default"],
        "switch.thumb.background": "#FFFFFF",
        "skeleton.background": t["bg/hover"],
        "progress.bar.background": t["accent"],
        "slider.background": t["accent"],
        "slider.thumb.background": "#FFFFFF",
        "drag.border": t["accent"],
        "drop_target.background": alpha(t["accent"], 0.12),
        "link": t["info"],
        "link.hover": SHADE["info"][i][0],
        "link.active": SHADE["info"][i][1],
        "accordion.background": t["bg/canvas"],
        "group_box.background": t["bg/sidebar"],
        "group_box.foreground": t["text/primary"],
        "group_box.title.foreground": t["text/secondary"],
        "description_list.label.background": t["bg/sidebar"],
        "description_list.label.foreground": t["text/secondary"],
        "window.border": t["border/default"],
        "chart.1": t["accent"],
        "chart.2": t["info"],
        "chart.3": t["success"],
        "chart.4": t["warning"],
        "chart.5": t["keyword"],
        "chart.bullish": t["success"],
        "chart.bearish": t["danger"],
        "chart.grid": t["border/subtle"],
    }
    for key, token in [("primary", "accent"), ("success", "success"), ("warning", "warning"),
                       ("danger", "danger"), ("info", "info")]:
        c[f"{key}.background"] = t[token]
        c[f"{key}.foreground"] = on_fill
        c[f"{key}.hover.background"], c[f"{key}.active.background"] = SHADE[token][i]
    for key, token in [("red", "danger"), ("green", "success"), ("blue", "info"),
                       ("yellow", "warning"), ("magenta", "keyword"), ("cyan", "number")]:
        c[f"base.{key}"] = t[token]
        c[f"base.{key}.light"] = alpha(t[token], 0.16)

    # SQL editor and code views: keyword violet, string green, number amber,
    # function blue, comment muted (design.md, Colour tokens).
    syntax = {
        "keyword": t["keyword"], "boolean": t["keyword"], "constant": t["keyword"],
        "string": t["success"], "string.escape": t["success"], "string.special": t["success"],
        "number": t["warning"], "function": t["info"], "attribute": t["info"],
        "comment": t["text/muted"], "comment.doc": t["text/muted"], "type": t["number"],
        "operator": t["text/secondary"], "punctuation": t["text/secondary"],
        "property": t["text/primary"], "variable": t["text/primary"], "title": t["text/primary"],
        "tag": t["accent"],
    }
    highlight = {
        "editor.background": t["bg/canvas"],
        "editor.foreground": t["text/primary"],
        "editor.active_line.background": t["bg/zebra"] if dark else "#F7F7F9",
        "editor.line_number": t["text/faint"],
        "editor.active_line_number": t["text/secondary"],
        "editor.invisible": alpha(t["text/faint"], 0.6),
        "conflict": t["danger"], "created": t["success"], "deleted": t["danger"],
        "modified": t["warning"], "hidden": t["text/muted"], "hint": t["keyword"],
        "predictive": t["text/faint"], "warning": t["warning"],
        "syntax": {k: {"color": v} for k, v in syntax.items()},
    }
    return {
        "name": name, "mode": mode,
        "font.family": "Geist", "mono_font.family": "Geist Mono",
        "radius": 6, "radius.lg": 12,
        "colors": c, "highlight": highlight,
    }


out = {
    "name": "Tabula",
    "author": "Generated by assets/themes/tabula.py; edit the tokens there.",
    "themes": [theme(0, "Tabula Dark", "dark"), theme(1, "Tabula Light", "light")],
    # The raw tokens, for colours gpui-component's theme has no slot for
    # (read by src/ui/theme.rs).
    "tokens": {mode: {k: v[i] for k, v in T.items()} for i, mode in enumerate(["dark", "light"])},
}
Path(__file__).with_name("tabula.json").write_text(json.dumps(out, indent=2) + "\n")
