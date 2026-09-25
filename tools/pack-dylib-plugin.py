#!/usr/bin/env python3
"""Build and package one trusted Linux dylib plugin against an immutable SDK."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tomllib

MAGIC = b"RUTIS_PLUGIN_BOOT_V1\0"
BOOT_SIZE = 512


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def elf_boot(path: Path) -> tuple[str, str, str, str]:
    data = path.read_bytes()
    if data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("expected little-endian ELF64")
    shoff = struct.unpack_from("<Q", data, 40)[0]
    shsize, count, names_index = struct.unpack_from("<HHH", data, 58)
    if shsize < 64 or not count or names_index >= count:
        raise ValueError("unsupported ELF section table")

    def section(index: int) -> tuple[int, int, int]:
        base = shoff + index * shsize
        if base + 64 > len(data):
            raise ValueError("truncated ELF section table")
        return (struct.unpack_from("<I", data, base)[0],
                struct.unpack_from("<Q", data, base + 24)[0],
                struct.unpack_from("<Q", data, base + 32)[0])

    _, name_offset, name_size = section(names_index)
    names = data[name_offset:name_offset + name_size]
    if len(names) != name_size:
        raise ValueError("truncated ELF name table")
    found = []
    for index in range(count):
        name, offset, size = section(index)
        if name >= len(names):
            raise ValueError("bad ELF section name")
        end = names.find(b"\0", name)
        if end < 0:
            raise ValueError("unterminated ELF section name")
        if names[name:end] == b".note.rutis.meta":
            found.append(data[offset:offset + size])
    if len(found) != 1 or len(found[0]) != BOOT_SIZE or not found[0].startswith(MAGIC):
        raise ValueError("expected one valid plugin boot section")
    boot = found[0]
    pos = len(MAGIC)
    values = []
    for _ in range(4):
        length = struct.unpack_from("<H", boot, pos)[0]
        pos += 2
        values.append(boot[pos:pos + length].decode("utf-8"))
        pos += length
        if pos > BOOT_SIZE:
            raise ValueError("truncated boot field")
    return tuple(values)


def lock_path(manifest: Path) -> Path:
    for directory in (manifest.parent, *manifest.parent.parents):
        candidate = directory / "Cargo.lock"
        if candidate.exists():
            return candidate
    raise ValueError("Cargo.lock not found")


def build(manifest: Path, target_dir: Path, sdk_hash: str, flags: str, features: str,
          anchor_package: str | None, anchor_features: list[str]) -> Path:
    command = ["cargo", "build", "--release", "--locked", "--manifest-path", str(manifest)]
    package = tomllib.loads(manifest.read_text())["package"]
    if anchor_package:
        command.extend(["-p", package["name"], "-p", anchor_package])
        selected = [f"{package['name']}/{feature}" for feature in features.split(",") if feature]
        selected.extend(f"{anchor_package}/{feature}" for feature in anchor_features)
    else:
        selected = [feature for feature in features.split(",") if feature]
    if selected:
        command.extend(["--features", ",".join(selected)])
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["RUSTFLAGS"] = flags
    env["RUTIS_SDK_ARTIFACT_SHA256"] = "0" * 64
    subprocess.run(command, env=env, check=True)
    sdk_file = target_dir / "release/librutis_sdk.so"
    if sha256(sdk_file) != sdk_hash:
        raise ValueError("independent SDK build differs from the published artifact; build the plugin in the SDK release pipeline")
    env["RUTIS_SDK_ARTIFACT_SHA256"] = sdk_hash
    subprocess.run(command, env=env, check=True)
    if sha256(sdk_file) != sdk_hash:
        raise ValueError("SDK artifact changed during the second build")
    library = tomllib.loads(manifest.read_text()).get("lib", {}).get("name", package["name"])
    return target_dir / "release" / ("lib" + library.replace("-", "_") + ".so")


def check_shared_duplicates(manifest: Path) -> None:
    result = subprocess.run(["cargo", "tree", "-d", "--locked", "--manifest-path", str(manifest)],
                            text=True, capture_output=True, check=True)
    for name in ("rutis", "tokio", "tokio-util", "serde_json"):
        if any(line.startswith(name + " v") for line in result.stdout.splitlines()):
            raise ValueError(f"duplicate shared dependency {name}; align versions with the SDK")


def check_allocator(library: Path) -> None:
    result = subprocess.run(["nm", "-D", "--defined-only", str(library)],
                            text=True, capture_output=True, check=True)
    if any(line.split()[-1] in ("__rust_alloc", "__rust_dealloc", "__rust_realloc")
           for line in result.stdout.splitlines() if line.split()):
        raise ValueError("plugin defines its own global allocator")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["pack-plugin"])
    parser.add_argument("--manifest-path", type=Path, required=True)
    parser.add_argument("--sdk-manifest", type=Path, required=True)
    parser.add_argument("--sdk-file", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--features", default="")
    parser.add_argument("--anchor-package")
    parser.add_argument("--anchor-features", default="")
    parser.add_argument("--prebuilt-library", type=Path)
    parser.add_argument("--interface", action="append", default=[])
    args = parser.parse_args()
    manifest = args.manifest_path.resolve()
    release = tomllib.loads(args.sdk_manifest.read_text())
    sdk = release["sdk"]
    if sha256(args.sdk_file) != sdk["artifact_sha256"]:
        raise ValueError("SDK file differs from SDK release manifest")
    package = tomllib.loads(manifest.read_text())["package"]
    check_shared_duplicates(manifest)
    if args.prebuilt_library:
        library = args.prebuilt_library.resolve()
    else:
        target_dir = (args.target_dir or manifest.parent / "target").resolve()
        repo = Path(__file__).resolve().parent.parent
        cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))).resolve()
        flags = os.environ.get("RUSTFLAGS", "")
        flags += (f" --remap-path-prefix={repo}=/src --remap-path-prefix={target_dir}=/target"
                  f" --remap-path-prefix={cargo_home}=/cargo -C link-arg=-Wl,-rpath,$ORIGIN")
        release_build = release.get("build", {})
        anchor_package = args.anchor_package or release_build.get("anchor_package")
        anchor_features = [f for f in args.anchor_features.split(",") if f] or release_build.get("anchor_features", [])
        library = build(manifest, target_dir, sdk["artifact_sha256"], flags, args.features,
                        anchor_package, anchor_features)
    check_allocator(library)
    sdk_id, artifact, plugin_id, version = elf_boot(library)
    if (sdk_id, artifact) != (sdk["id"], sdk["artifact_sha256"]):
        raise ValueError("plugin boot identity differs from the published SDK")
    if version != package["version"]:
        raise ValueError("plugin boot version differs from Cargo.toml")
    interfaces = {}
    for item in args.interface:
        name, requirement = item.split("=", 1)
        interfaces[name] = requirement
    library_sha = sha256(library)
    rustc = subprocess.check_output(["rustc", "--version"], text=True).strip()
    if rustc != sdk["rustc"]:
        raise ValueError("plugin rustc differs from the published SDK")
    locked_sha = sha256(lock_path(manifest))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    name = "lib" + plugin_id.replace("-", "_") + ".so"
    shutil.copy2(library, output / name)
    if sha256(output / name) != library_sha:
        raise ValueError("library changed while packaging")
    content = ("[plugin]\n"
               f"id = {json.dumps(plugin_id)}\nversion = {json.dumps(version)}\n"
               f"library = {json.dumps(name)}\nlibrary_sha256 = {json.dumps(library_sha)}\n\n"
               "[sdk]\n"
               f"version = {json.dumps(sdk['version'])}\nid = {json.dumps(sdk_id)}\n"
               f"artifact_sha256 = {json.dumps(artifact)}\n\n"
               "[interfaces]\n" + "".join(f"{json.dumps(k)} = {json.dumps(v)}\n" for k, v in sorted(interfaces.items())) + "\n"
               "[build]\n"
               f"target = {json.dumps(sdk['target'])}\nrustc = {json.dumps(rustc)}\n"
               f"lock_sha256 = {json.dumps(locked_sha)}\n")
    (output / "plugin.toml").write_text(content)
    print(output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError, KeyError, struct.error) as error:
        print(f"pack-plugin: {error}", file=sys.stderr)
        sys.exit(1)
