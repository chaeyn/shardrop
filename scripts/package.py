#!/usr/bin/env python3
"""Maintainer-only native release packaging; never needed by end users."""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--generator', type=Path)
parser.add_argument('--target', required=True)
parser.add_argument('--output', type=Path, default=Path('dist'))
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
version = tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']
name = f'shardrop-v{version}-{args.target}'
args.output.mkdir(parents=True, exist_ok=True)
generator = (args.generator or args.binary).resolve()
actual = subprocess.check_output([str(generator), '--version'], text=True).strip()
if actual != f'shardrop {version}':
    raise SystemExit(f'Generator version mismatch: {actual}')
with tempfile.TemporaryDirectory() as temporary:
    staging = Path(temporary) / name
    staging.mkdir()
    exe = 'shardrop.exe' if 'windows' in args.target else 'shardrop'
    shutil.copy2(args.binary, staging / exe)
    for file in ['LICENSE', 'README.md', 'README.ko.md', 'CHANGELOG.md', 'SECURITY.md']:
        shutil.copy2(root / file, staging / file)
    shutil.copytree(root / 'docs', staging / 'docs')
    shutil.copy2(root / 'scripts/install-local.sh', staging / 'install-local.sh')
    shutil.copy2(root / 'scripts/install-local.ps1', staging / 'install-local.ps1')
    (staging / 'completions').mkdir()
    for shell in ['bash', 'zsh', 'fish', 'powershell', 'elvish']:
        data = subprocess.check_output([str(generator), 'completions', shell])
        (staging / 'completions' / f'shardrop.{shell}').write_bytes(data)
    (staging / 'shardrop.1').write_bytes(subprocess.check_output([str(generator), 'man']))
    if 'windows' in args.target:
        archive = args.output / f'{name}.zip'
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as output:
            for file in sorted(staging.rglob('*')):
                if file.is_file():
                    output.write(file, file.relative_to(staging.parent))
    else:
        archive = args.output / f'{name}.tar.gz'
        with tarfile.open(archive, 'w:gz') as output:
            output.add(staging, arcname=name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + '.sha256').write_text(f'{digest}  {archive.name}\n')
    print(archive)
