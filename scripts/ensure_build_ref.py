#!/usr/bin/env python3
"""Preserve the run's commit before concurrent pushes move its branch head.

GitHub's workflow-scoped release restriction rejects unreferenced commits that
change workflow files. A lightweight build tag lets the ordinary contents:write
workflow token publish the tested SHA later, without a broader credential.
"""
import json
import os
import re
import subprocess


def main():
    sha = os.environ['GITHUB_SHA']
    repository = os.environ['GITHUB_REPOSITORY']
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise SystemExit('Expected the exact GitHub run commit SHA')
    tag = 'build-' + sha[:12]
    result = subprocess.run(['gh', 'api', f'repos/{repository}/git/ref/tags/{tag}'],
                            capture_output=True, text=True)
    if result.returncode == 0:
        if json.loads(result.stdout)['object']['sha'] != sha:
            raise SystemExit('Build tag belongs to another commit; refusing to move it')
        print('Build commit already preserved: ' + tag)
        return
    if 'HTTP 404' not in result.stderr:
        raise SystemExit('Could not inspect build reference: ' + result.stderr)
    subprocess.run(['gh', 'api', '--method', 'POST', f'repos/{repository}/git/refs',
                    '-f', 'ref=refs/tags/' + tag, '-f', 'sha=' + sha], check=True,
                   stdout=subprocess.DEVNULL)
    print('Preserved build commit: ' + tag)


if __name__ == '__main__':
    main()
