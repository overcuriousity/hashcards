#!/usr/bin/env python3
"""
Extract the latest release from CHANGELOG.xml and format as Markdown, or
promote its <unreleased> section into a new dated <release>.
"""

import datetime
import xml.etree.ElementTree as ET
import sys
from pathlib import Path


def extract_latest_release(changelog_path: Path) -> tuple[str, str]:
    tree = ET.parse(changelog_path)
    root = tree.getroot()

    # Get the first release element
    release = root.find("./releases/release")

    if release is None:
        print("Error: No releases found in CHANGELOG.xml", file=sys.stderr)
        sys.exit(1)

    version = release.get("version")
    date = release.get("date")

    if version is None:
        print("Error: Release missing version attribute", file=sys.stderr)
        sys.exit(1)

    if date is None:
        print("Error: Release missing data attribute", file=sys.stderr)
        sys.exit(1)

    # Build markdown content
    lines: list[str] = []

    lines.append(f"- **Version:** {version}")
    lines.append(f"- **Date:** {date}")
    lines.append("")

    # Category mapping to nice headers
    categories = {
        "added": "## Added",
        "fixed": "## Fixed",
        "changed": "## Changed",
        "removed": "## Removed",
        "deprecated": "## Deprecated",
        "security": "## Security",
        "breaking": "## Breaking Changes",
    }

    for category_tag, header in categories.items():
        category = release.find(category_tag)
        if category is not None:
            changes = category.findall("change")
            if changes:
                lines.append(header)
                lines.append("")
                for change in changes:
                    text = change.text.strip() if change.text else ""
                    lines.append(f"- {text}")
                lines.append("")

    # Remove trailing empty line
    if lines and lines[-1] == "":
        _ = lines.pop()

    markdown = "\n".join(lines)

    return version, markdown


def has_unreleased_changes(changelog_path: Path) -> bool:
    root = ET.parse(changelog_path).getroot()
    unreleased = root.find("./unreleased")
    return unreleased is not None and len(list(unreleased)) > 0


def promote_unreleased(changelog_path: Path) -> str:
    """Move <unreleased>'s contents into a new <release>, and return its version.

    The next version is derived from the latest existing release: a
    <breaking> section bumps the minor component (this project is pre-1.0,
    so a major bump is never automatic — see the 0.1.2 -> 0.2.0 precedent in
    CHANGELOG.xml), otherwise the patch component is bumped. This is a
    convention, not a strict rule: past releases have used judgement calls
    the version number doesn't fully capture.
    """
    tree = ET.parse(changelog_path)
    root = tree.getroot()

    unreleased = root.find("./unreleased")
    if unreleased is None or len(list(unreleased)) == 0:
        print("Error: no unreleased changes to promote", file=sys.stderr)
        sys.exit(1)

    releases = root.find("./releases")
    latest = releases.find("./release") if releases is not None else None
    if releases is None or latest is None:
        print("Error: no prior release found in CHANGELOG.xml", file=sys.stderr)
        sys.exit(1)

    major, minor, patch = (int(p) for p in latest.get("version").split(".")[:3])
    if unreleased.find("breaking") is not None:
        new_version = f"{major}.{minor + 1}.0"
    else:
        new_version = f"{major}.{minor}.{patch + 1}"

    new_release = ET.Element(
        "release",
        {"version": new_version, "date": datetime.date.today().isoformat()},
    )
    for child in list(unreleased):
        unreleased.remove(child)
        new_release.append(child)
    releases.insert(0, new_release)

    ET.indent(tree, space="    ")
    tree.write(changelog_path, encoding="unicode")
    changelog_path.write_text(changelog_path.read_text().rstrip("\n") + "\n")

    return new_version


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("Usage: release.py <command>", file=sys.stderr)
        sys.exit(1)

    command: str = sys.argv[1]
    changelog_path = Path("CHANGELOG.xml")

    if command == "version":
        version, _ = extract_latest_release(changelog_path)
        print(version)
    elif command == "markdown":
        _, markdown = extract_latest_release(changelog_path)
        print(markdown)
    elif command == "has-unreleased":
        print("true" if has_unreleased_changes(changelog_path) else "false")
    elif command == "bump":
        print(promote_unreleased(changelog_path))
    else:
        print(f"Unknown command: {command}", file=sys.stderr)
        sys.exit(1)
