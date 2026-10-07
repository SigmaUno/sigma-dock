#!/usr/bin/env python3
"""Preserve the run's commit before concurrent pushes move its branch head.

GitHub's workflow-scoped release restriction rejects unreferenced commits that
change workflow files. The exact release tag lets the ordinary contents:write
workflow token publish the tested SHA later, without a broader credential.
"""
import argparse
import json
import os
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'(?:macos-[0-9a-f]{12}|v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)', args.tag):
        raise SystemExit('Expected a macOS snapshot or semantic version release tag')
    sha = os.environ['GITHUB_SHA']
    repository = os.environ['GITHUB_REPOSITORY']
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise SystemExit('Expected the exact GitHub run commit SHA')
    tag = args.tag
    result = subprocess.run(['gh', 'api', f'repos/{repository}/git/ref/tags/{tag}'],
                            capture_output=True, text=True)
    if result.returncode == 0:
        target = json.loads(result.stdout)['object']
        actual = target['sha']
        if target['type'] == 'tag':
            actual = subprocess.check_output(['gh', 'api', f'repos/{repository}/commits/{tag}', '--jq', '.sha'], text=True).strip()
        if actual != sha:
            raise SystemExit('Release tag belongs to another commit; refusing to move it')
        print('Release commit already preserved: ' + tag)
        return
    if 'HTTP 404' not in result.stderr:
        raise SystemExit('Could not inspect build reference: ' + result.stderr)
    subprocess.run(['gh', 'api', '--method', 'POST', f'repos/{repository}/git/refs',
                    '-f', 'ref=refs/tags/' + tag, '-f', 'sha=' + sha], check=True,
                   stdout=subprocess.DEVNULL)
    print('Preserved release commit: ' + tag)


if __name__ == '__main__':
    main()
