import argparse
import email.parser
import subprocess
import tempfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROJECTS = ("adapter-protocol", "spacy-adapter", "stanza-adapter")
ENVIRONMENTS = {
    "spacy-adapter": ("requirements-ginza.txt", ("spacy", "ginza")),
    "stanza-adapter": ("requirements-cpu.txt", ("cpu",)),
}


def check_exports(update=False):
    for project, (name, extras) in ENVIRONMENTS.items():
        subprocess.run(
            ["uv", "lock", "--project", str(ROOT / "python" / project), "--check"],
            check=True,
        )
        command = [
            "uv",
            "export",
            "--project",
            str(ROOT / "python" / project),
            "--frozen",
            "--no-dev",
            "--no-emit-local",
            "--no-header",
            "--no-annotate",
        ]
        for extra in extras:
            command.extend(("--extra", extra))
        exported = subprocess.run(command, check=True, capture_output=True).stdout
        path = ROOT / "python" / project / name
        if update:
            path.write_bytes(exported)
        require(
            exported == path.read_bytes(),
            f"{project}/{name} differs from uv.lock; run this script with --update-exports",
        )


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_versions(tag=None):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"][
        "version"
    ]
    if tag is not None:
        require(tag == f"v{version}", f"tag {tag} differs from workspace v{version}")
    for project in PROJECTS:
        metadata = tomllib.loads(
            (ROOT / "python" / project / "pyproject.toml").read_text()
        )["project"]
        require(
            metadata["version"] == version,
            f"{project}: version {metadata['version']} differs from workspace {version}",
        )
        if project != "adapter-protocol":
            require(
                f"lingrep-adapter-protocol=={version}" in metadata["dependencies"],
                f"{project}: protocol dependency must pin version {version}",
            )


def check_wheel(directory, project):
    source = ROOT / "python" / project
    metadata = tomllib.loads((source / "pyproject.toml").read_text())
    name = metadata["project"]["name"].replace("-", "_")
    version = metadata["project"]["version"]
    wheel = directory / f"{name}-{version}-py3-none-any.whl"
    with zipfile.ZipFile(wheel) as archive:
        files = archive.namelist()
        expected = {
            f"{name}/{path.name}": path.read_bytes()
            for path in (source / "src" / name).glob("*.py")
        }
        dist_info = f"{name}-{version}.dist-info"
        expected[f"{dist_info}/licenses/LICENSE"] = (ROOT / "LICENSE").read_bytes()
        expected[f"{name}/_project/pyproject.toml"] = (
            source / "pyproject.toml"
        ).read_bytes()
        if project != "adapter-protocol":
            requirements = ENVIRONMENTS[project][0]
            expected[f"{name}/{requirements}"] = (source / requirements).read_bytes()
            expected[f"{name}/uv.lock"] = (source / "uv.lock").read_bytes()
            expected[f"{name}/_project/bin/lingrep-{project}"] = (
                source / "bin" / f"lingrep-{project}"
            ).read_bytes()
        for path, content in expected.items():
            require(archive.read(path) == content, f"{wheel.name}: {path} differs")
        metadata_files = {
            f"{dist_info}/{path}" for path in ("METADATA", "WHEEL", "RECORD")
        }
        if project != "adapter-protocol":
            metadata_files.add(f"{dist_info}/entry_points.txt")
        require(len(files) == len(set(files)), f"{wheel.name}: duplicate paths")
        require(
            set(files) == expected.keys() | metadata_files,
            f"{wheel.name}: unexpected or missing paths: "
            f"{set(files) ^ (expected.keys() | metadata_files)}",
        )
        packaged = email.parser.Parser().parsestr(
            archive.read(f"{dist_info}/METADATA").decode()
        )
        require(
            set(packaged["Requires-Python"].split(","))
            == set(metadata["project"]["requires-python"].split(",")),
            f"{wheel.name}: Requires-Python differs",
        )
        require(
            packaged["License-Expression"] == "MIT", f"{wheel.name}: invalid license"
        )
        require(
            packaged.get_all("License-File") == ["LICENSE"],
            f"{wheel.name}: missing License-File metadata",
        )
        require(packaged["Version"] == version, f"{wheel.name}: Version differs")
        tags = email.parser.Parser().parsestr(
            archive.read(f"{dist_info}/WHEEL").decode()
        )
        require(tags.get_all("Tag") == ["py3-none-any"], f"{wheel.name}: invalid tags")
        require(tags["Root-Is-Purelib"] == "true", f"{wheel.name}: not pure Python")
    return wheel


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path)
    parser.add_argument("--tag")
    parser.add_argument(
        "--update-exports",
        action="store_true",
        help="Regenerate runtime requirements from the adapter locks.",
    )
    args = parser.parse_args()
    check_versions(args.tag)
    check_exports(args.update_exports)
    with tempfile.TemporaryDirectory(prefix="lingrep-wheel-check-") as temporary:
        directory = args.output.resolve() if args.output else Path(temporary) / "dist"
        for project in PROJECTS:
            subprocess.run(
                [
                    "uv",
                    "build",
                    "--wheel",
                    "--out-dir",
                    str(directory),
                    str(ROOT / "python" / project),
                ],
                check=True,
            )
        wheels = {project: check_wheel(directory, project) for project in PROJECTS}
        environment = Path(temporary) / "environment"
        subprocess.run(
            ["uv", "venv", "--python", "3.14", str(environment)],
            check=True,
        )
        subprocess.run(
            [
                "uv",
                "pip",
                "install",
                "--python",
                str(environment / "bin/python"),
                "--no-deps",
                *(str(wheel) for wheel in wheels.values()),
            ],
            check=True,
        )
        for provider in ("spacy", "stanza"):
            subprocess.run(
                [str(environment / f"bin/lingrep-{provider}-setup"), "--help"],
                cwd=temporary,
                check=True,
            )


if __name__ == "__main__":
    main()
