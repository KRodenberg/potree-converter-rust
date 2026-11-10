"""Rebuild the embedded, licensed Potree runtime from a local libs directory."""
from pathlib import Path
import re
import sys
import zipfile

source = Path(sys.argv[1]).resolve()
target = Path(__file__).resolve().parents[1] / "assets" / "viewer-libs.zip"
with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for file in sorted(source.rglob("*")):
        if not file.is_file() or file.name.casefold() in {
            "adwjlogohires.png", "laz-potree-template.html"
        }:
            continue
        relative = file.relative_to(source).as_posix()
        data = file.read_bytes()
        if relative == "potree/sidebar.html":
            text = data.decode("utf-8-sig")
            text = re.sub(r'\s*<a href="http://www\.ADWJohnson\.com\.au".*?</a>', "", text, flags=re.S)
            data = text.encode("utf-8")
        info = zipfile.ZipInfo(relative, date_time=(2026, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        archive.writestr(info, data)
print(f"Bundled {target.name}: {target.stat().st_size:,} bytes")
