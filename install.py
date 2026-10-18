"""Install the launcher and update the existing local model configs and Fish abbreviations."""

import datetime
import json
import re
import shutil
from pathlib import Path


def install():
    home = Path.home()
    executable = Path(__file__).resolve().parent / "strata-wrap"
    link = home / ".local/bin/strata-wrap"
    if (link.exists() or link.is_symlink()) and (
        not link.is_symlink() or link.resolve() != executable
    ):
        raise SystemExit(f"{link} already exists and points somewhere else")
    fish = home / ".config/fish/.config.local.fish"
    configs = [
        home / "apps/Strata" / f"strata-{model}.json" for model in ("iq3_s", "ud-iq3_s")
    ]
    changes = {}
    for config in configs:
        data = json.loads(config.read_text())
        data.update(vram_elastic=True, vram_segment_mib=512)
        changes[config] = json.dumps(data, indent=2) + "\n"
    text = fish.read_text()
    for abbreviation, model in (("sa", "iq3_s"), ("saud", "ud-iq3_s")):
        text, count = re.subn(
            rf"^abbr --add {abbreviation} .*?$",
            f"abbr --add {abbreviation} '$HOME/.local/bin/strata-wrap {model}'",
            text,
            flags=re.MULTILINE,
        )
        if count != 1:
            raise SystemExit(
                f"expected exactly one {abbreviation} abbreviation in {fish}"
            )
    changes[fish] = text
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    backup = home / ".local/state/strata-wrap/backups" / stamp
    backup.mkdir(parents=True, mode=0o700)
    for path, text in changes.items():
        shutil.copy2(path, backup / path.name)
        path.write_text(text)
    link.parent.mkdir(parents=True, exist_ok=True)
    if not link.is_symlink():
        link.symlink_to(executable)
    print(f"Installed {link}")
    print(
        f"Enabled elastic mode in both configs; updated sa and saud. Backups: {backup}"
    )
    print("Open a new Fish shell or run: source ~/.config/fish/.config.local.fish")


if __name__ == "__main__":
    install()
