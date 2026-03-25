#!/usr/bin/env python3
"""Stage and optionally package the dcode npm module."""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
DCODE_CLI_ROOT = SCRIPT_DIR.parent
REPO_ROOT = DCODE_CLI_ROOT.parent
DCODE_NPM_NAME = "dcode"

# Platform-specific optional dependency packages.
# `npm_name` is what bin/dcode.js imports at runtime.
DCODE_PLATFORM_PACKAGES: dict[str, dict[str, str]] = {
    "dcode-linux-x64": {
        "npm_name": "dcode-linux-x64",
        "npm_tag": "linux-x64",
        "target_triple": "x86_64-unknown-linux-musl",
        "os": "linux",
        "cpu": "x64",
    },
    "dcode-linux-arm64": {
        "npm_name": "dcode-linux-arm64",
        "npm_tag": "linux-arm64",
        "target_triple": "aarch64-unknown-linux-musl",
        "os": "linux",
        "cpu": "arm64",
    },
    "dcode-darwin-x64": {
        "npm_name": "dcode-darwin-x64",
        "npm_tag": "darwin-x64",
        "target_triple": "x86_64-apple-darwin",
        "os": "darwin",
        "cpu": "x64",
    },
    "dcode-darwin-arm64": {
        "npm_name": "dcode-darwin-arm64",
        "npm_tag": "darwin-arm64",
        "target_triple": "aarch64-apple-darwin",
        "os": "darwin",
        "cpu": "arm64",
    },
    "dcode-win32-x64": {
        "npm_name": "dcode-win32-x64",
        "npm_tag": "win32-x64",
        "target_triple": "x86_64-pc-windows-msvc",
        "os": "win32",
        "cpu": "x64",
    },
    "dcode-win32-arm64": {
        "npm_name": "dcode-win32-arm64",
        "npm_tag": "win32-arm64",
        "target_triple": "aarch64-pc-windows-msvc",
        "os": "win32",
        "cpu": "arm64",
    },
}

PACKAGE_EXPANSIONS: dict[str, list[str]] = {
    "dcode": ["dcode", *DCODE_PLATFORM_PACKAGES],
}

PACKAGE_NATIVE_COMPONENTS: dict[str, list[str]] = {
    "dcode": [],
    "dcode-linux-x64": ["dcode", "rg"],
    "dcode-linux-arm64": ["dcode", "rg"],
    "dcode-darwin-x64": ["dcode", "rg"],
    "dcode-darwin-arm64": ["dcode", "rg"],
    "dcode-win32-x64": ["dcode", "rg"],
    "dcode-win32-arm64": ["dcode", "rg"],
}

PACKAGE_TARGET_FILTERS: dict[str, str] = {
    package_name: package_config["target_triple"]
    for package_name, package_config in DCODE_PLATFORM_PACKAGES.items()
}

PACKAGE_CHOICES = tuple(PACKAGE_NATIVE_COMPONENTS)

COMPONENT_DEST_DIR: dict[str, str] = {
    "dcode": "dcode",
    "rg": "path",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build or stage the dcode CLI npm package.")
    parser.add_argument(
        "--package",
        choices=PACKAGE_CHOICES,
        default="dcode",
        help="Which npm package to stage (default: dcode).",
    )
    parser.add_argument(
        "--version",
        help="Version number to write to package.json inside the staged package.",
    )
    parser.add_argument(
        "--release-version",
        help="Version to stage for npm release.",
    )
    parser.add_argument(
        "--staging-dir",
        type=Path,
        help=(
            "Directory to stage the package contents. Defaults to a new temporary directory "
            "if omitted. The directory must be empty when provided."
        ),
    )
    parser.add_argument(
        "--pack-output",
        type=Path,
        help="Path where the generated npm tarball should be written.",
    )
    parser.add_argument(
        "--vendor-src",
        type=Path,
        help="Directory containing pre-installed native binaries to bundle (vendor root).",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    package = args.package
    version = args.version
    release_version = args.release_version
    if release_version:
        if version and version != release_version:
            raise RuntimeError("--version and --release-version must match when both are provided.")
        version = release_version

    if not version:
        raise RuntimeError("Must specify --version or --release-version.")

    staging_dir, created_temp = prepare_staging_dir(args.staging_dir)

    try:
        stage_sources(staging_dir, version, package)

        vendor_src = args.vendor_src.resolve() if args.vendor_src else None
        native_components = PACKAGE_NATIVE_COMPONENTS.get(package, [])
        target_filter = PACKAGE_TARGET_FILTERS.get(package)

        if native_components:
            if vendor_src is None:
                components_str = ", ".join(native_components)
                raise RuntimeError(
                    "Native components "
                    f"({components_str}) required for package '{package}'. Provide --vendor-src "
                    "pointing to a directory containing pre-installed binaries."
                )

            copy_native_binaries(
                vendor_src,
                staging_dir,
                native_components,
                target_filter={target_filter} if target_filter else None,
            )

        if release_version:
            staging_dir_str = str(staging_dir)
            if package == "dcode":
                print(
                    f"Staged version {version} for release in {staging_dir_str}\n\n"
                    "Verify the CLI:\n"
                    f"    node {staging_dir_str}/bin/dcode.js --version\n"
                    f"    node {staging_dir_str}/bin/dcode.js --help\n\n"
                )
            elif package in DCODE_PLATFORM_PACKAGES:
                print(
                    f"Staged version {version} for release in {staging_dir_str}\n\n"
                    "Verify native payload contents:\n"
                    f"    ls {staging_dir_str}/vendor\n\n"
                )
            else:
                print(f"Staged package in {staging_dir}")
        else:
            print(f"Staged package in {staging_dir}")

        if args.pack_output is not None:
            output_path = run_npm_pack(staging_dir, args.pack_output)
            print(f"npm pack output written to {output_path}")
    finally:
        if created_temp:
            # Preserve the staging directory for further inspection.
            pass

    return 0


def prepare_staging_dir(staging_dir: Path | None) -> tuple[Path, bool]:
    if staging_dir is not None:
        staging_dir = staging_dir.resolve()
        staging_dir.mkdir(parents=True, exist_ok=True)
        if any(staging_dir.iterdir()):
            raise RuntimeError(f"Staging directory {staging_dir} is not empty.")
        return staging_dir, False

    temp_dir = Path(tempfile.mkdtemp(prefix="dcode-npm-stage-"))
    return temp_dir, True


def stage_sources(staging_dir: Path, version: str, package: str) -> None:
    package_json: dict
    package_json_path: Path | None = None

    if package == "dcode":
        bin_dir = staging_dir / "bin"
        bin_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(DCODE_CLI_ROOT / "bin" / "dcode.js", bin_dir / "dcode.js")
        rg_manifest = DCODE_CLI_ROOT / "bin" / "rg"
        if rg_manifest.exists():
            shutil.copy2(rg_manifest, bin_dir / "rg")

        readme_src = DCODE_CLI_ROOT / "README.md"
        if readme_src.exists():
            shutil.copy2(readme_src, staging_dir / "README.md")

        package_json_path = DCODE_CLI_ROOT / "package.json"
    elif package in DCODE_PLATFORM_PACKAGES:
        platform_package = DCODE_PLATFORM_PACKAGES[package]
        platform_version = compute_platform_package_version(version, platform_package["npm_tag"])

        readme_src = DCODE_CLI_ROOT / "README.md"
        if readme_src.exists():
            shutil.copy2(readme_src, staging_dir / "README.md")

        with open(DCODE_CLI_ROOT / "package.json", "r", encoding="utf-8") as fh:
            dcode_package_json = json.load(fh)

        package_json = {
            "name": platform_package["npm_name"],
            "version": platform_version,
            "description": f"dcode native binary for {platform_package['os']}-{platform_package['cpu']}",
            "license": dcode_package_json.get("license", "Apache-2.0"),
            "os": [platform_package["os"]],
            "cpu": [platform_package["cpu"]],
            "files": ["vendor"],
            "repository": dcode_package_json.get("repository"),
        }

        engines = dcode_package_json.get("engines")
        if isinstance(engines, dict):
            package_json["engines"] = engines
    else:
        raise RuntimeError(f"Unknown package '{package}'.")

    if package_json_path is not None:
        with open(package_json_path, "r", encoding="utf-8") as fh:
            package_json = json.load(fh)
        package_json["version"] = version

    if package == "dcode":
        package_json["files"] = ["bin"]
        package_json["optionalDependencies"] = {
            DCODE_PLATFORM_PACKAGES[platform_pkg]["npm_name"]: (
                compute_platform_package_version(version, DCODE_PLATFORM_PACKAGES[platform_pkg]["npm_tag"])
            )
            for platform_pkg in PACKAGE_EXPANSIONS["dcode"]
            if platform_pkg != "dcode"
        }

    with open(staging_dir / "package.json", "w", encoding="utf-8") as out:
        json.dump(package_json, out, indent=2)
        out.write("\n")


def compute_platform_package_version(version: str, platform_tag: str) -> str:
    # npm forbids republishing the same package name/version, so each
    # platform-specific tarball needs a unique version string.
    return f"{version}-{platform_tag}"


def run_command(cmd: list[str], cwd: Path | None = None) -> None:
    print("+", " ".join(cmd))
    subprocess.run(cmd, cwd=cwd, check=True)


def copy_native_binaries(
    vendor_src: Path,
    staging_dir: Path,
    components: list[str],
    target_filter: set[str] | None,
) -> None:
    """Copy pre-built native binaries from vendor_src into staging_dir/vendor/."""
    vendor_dst = staging_dir / "vendor"

    for target_dir in vendor_src.iterdir():
        if not target_dir.is_dir():
            continue
        target_triple = target_dir.name
        if target_filter and target_triple not in target_filter:
            continue

        for component in components:
            dest_subdir = COMPONENT_DEST_DIR.get(component, component)
            src_component_dir = target_dir / component
            if not src_component_dir.exists():
                print(
                    f"Warning: missing component '{component}' for target '{target_triple}' "
                    f"in vendor_src. Skipping.",
                    file=sys.stderr,
                )
                continue

            dst_component_dir = vendor_dst / target_triple / dest_subdir
            dst_component_dir.mkdir(parents=True, exist_ok=True)

            for item in src_component_dir.iterdir():
                dst = dst_component_dir / item.name
                if item.is_file():
                    shutil.copy2(item, dst)
                    if not item.name.endswith((".dll", ".lib", ".pdb")):
                        dst.chmod(dst.stat().st_mode | 0o111)
                elif item.is_dir():
                    shutil.copytree(item, dst, dirs_exist_ok=True)


def run_npm_pack(staging_dir: Path, pack_output: Path) -> Path:
    pack_output.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        ["npm", "pack", "--json"],
        cwd=staging_dir,
        check=True,
        capture_output=True,
        text=True,
    )
    pack_result = json.loads(result.stdout)
    generated_tarball = staging_dir / pack_result[0]["filename"]
    shutil.move(str(generated_tarball), pack_output)
    return pack_output


if __name__ == "__main__":
    sys.exit(main())
