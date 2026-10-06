#!/usr/bin/env python3
"""Keep workspace versions aligned and publish missing packages on retry."""
import argparse
from collections import deque
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def metadata():
    return json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=ROOT))['packages']


def check(version):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version):
        raise SystemExit('Expected a semantic version, such as 0.1.1')
    packages = metadata()
    names = {p['name'] for p in packages}
    for p in packages:
        if p['version'] != version:
            raise SystemExit(f"{p['name']} is {p['version']}, expected {version}; run the bump helper first")
        for dep in p['dependencies']:
            if dep['name'] in names and dep['req'] != '^' + version:
                raise SystemExit(f"Internal dependency version mismatch: {p['name']} -> {dep['name']}")
    return packages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'bump', 'publish'])
    parser.add_argument('version')
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', args.version):
        parser.error('Expected a semantic version')
    if args.command == 'bump':
        packages = metadata()
        old = packages[0]['version']
        check(old)
        root_manifest = ROOT / 'Cargo.toml'
        root_manifest.write_text(root_manifest.read_text().replace(
            f'version = "{old}"', f'version = "{args.version}"', 1))
        for package in packages:
            manifest = Path(package['manifest_path'])
            text = manifest.read_text()
            text = re.sub(r'(sigma-dock[^\s=]*\s*=\s*\{\s*version\s*=\s*")' + re.escape(old) + r'(")',
                          lambda match: match[1] + args.version + match[2], text)
            manifest.write_text(text)
        subprocess.run(['cargo', 'generate-lockfile'], cwd=ROOT, check=True)
    packages = check(args.version)
    if args.command != 'publish':
        print(f"All {len(packages)} packages use {args.version}")
        return
    if not os.environ.get('CARGO_REGISTRY_TOKEN'):
        raise SystemExit('CARGO_REGISTRY_TOKEN is required for publishing')
    for attempt in range(4):
        command = ['cargo', 'publish', '--workspace', '--locked']
        missing = []
        for package in packages:
            name = package['name']
            url = f'https://crates.io/api/v1/crates/{name}/{args.version}'
            request = urllib.request.Request(url, headers={'User-Agent': 'SigmaDock-release (github.com/SigmaUno/sigma-dock)'})
            try:
                with urllib.request.urlopen(request, timeout=30) as response:
                    json.load(response)
                print(f'Already published: {name} {args.version}', flush=True)
                command.extend(['--exclude', name])
            except urllib.error.HTTPError as error:
                if error.code != 404:
                    raise
                missing.append(name)
        if not missing:
            print('All packages are published; no upload needed.')
            return
        print('Publishing: ' + ', '.join(missing), flush=True)
        tail = deque(maxlen=100)
        with subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True) as process:
            for line in process.stdout:
                print(line, end='', flush=True)
                tail.append(line)
            result = process.wait()
        if result == 0:
            return
        output = ''.join(tail)
        retry = re.search(r'Please try again after ([^\n]+? GMT)', output)
        if '429 Too Many Requests' not in output or not retry or attempt == 3:
            raise SystemExit(result)
        deadline = parsedate_to_datetime(retry[1]).timestamp() + 5
        while time.time() < deadline:
            remaining = deadline - time.time()
            print(f'Crates.io rate limit: retry in {remaining:.0f}s '
                  f'(after {datetime.fromtimestamp(deadline, timezone.utc).isoformat()})', flush=True)
            time.sleep(min(60, remaining))



if __name__ == '__main__':
    main()
